//! Cliente SMB2/3 via TCP, sem encaminhar credenciais em referrals DFS.

use super::{Entry, Host, MAX_DEPTH, Progress, Res, Step, check_cancel, err, join, normalize};
use smolder_core::prelude::{
    Client, CompoundRequest, CoreError, NtlmCredentials, SecurityPolicy, Share, SrvsvcClient,
};
use smolder_proto::smb::{
    smb2::{
        CloseRequest, Command, CreateDisposition, CreateOptions, CreateRequest, CreateResponse,
        Dialect, DispositionInformation, FileAttributes, FileId, FileInfoClass, FlushRequest,
        QueryDirectoryFlags, QueryDirectoryRequest, QueryDirectoryResponse, QueryInfoRequest,
        QueryInfoResponse, ReadRequest, RenameInformation, SetInfoRequest, ShareAccess,
        WriteRequest,
    },
    status::NtStatus,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{MappedMutexGuard, Mutex, MutexGuard},
};

pub(super) const PORT: u16 = 445;
const BLOCK: usize = 64 * 1024;
// Directory pages requested per round trip (each costs one credit and up to BLOCK bytes).
const PAGES_PER_TRIP: usize = 16;
// Access, create and attribute flags from MS-SMB2 / MS-FSCC.
const READ_DATA: u32 = 0x0000_0001;
const WRITE_DATA: u32 = 0x0000_0002;
const READ_ATTRIBUTES: u32 = 0x0000_0080;
const DELETE: u32 = 0x0001_0000;
const OPEN_REPARSE_POINT: CreateOptions = CreateOptions::from_bits_retain(0x0020_0000);
const REPARSE_POINT: FileAttributes = FileAttributes::from_bits_retain(0x0000_0400);
const NORMALIZED_NAME: u8 = 0x30; // FileNormalizedNameInformation
const FILE_NAME: u8 = 0x09; // FileNameInformation
const ATTRIBUTE_TAG: u8 = 0x23; // FileAttributeTagInformation
const FULL_DIRECTORY: u8 = 0x02; // FileFullDirectoryInformation
const REPARSE_LINK: &str = "SMB: refusing to follow a reparse point";
const UNVERIFIED_PATH: &str =
    "SMB: the server cannot verify the opened path; refusing to modify it";

#[cfg(test)]
tokio::task_local! {
    pub(super) static LEGACY_NAME_STATUS: ();
}

pub(super) struct Session {
    client: Client,
    root: Mutex<Option<SrvsvcClient>>,
    tree: Mutex<Option<Share>>,
}

impl Session {
    pub(super) async fn connect(host: &Host, port: u16) -> Res<Self> {
        let (domain, user) = host
            .user
            .split_once('\\')
            .unwrap_or((&host.domain, &host.user));
        let client = Client::builder(&host.hostname)
            .with_port(port)
            .with_security_policy(
                SecurityPolicy::credentialed().with_minimum_dialect(Dialect::Smb202),
            )
            .with_dialects(vec![
                Dialect::Smb202,
                Dialect::Smb210,
                Dialect::Smb300,
                Dialect::Smb302,
                Dialect::Smb311,
            ])
            .with_ntlm_credentials(NtlmCredentials::new(user, &host.password).with_domain(domain))
            .build()
            .map_err(err)?;
        let root = client.connect_srvsvc().await.map_err(err)?;
        Ok(Self {
            client,
            root: Mutex::new(Some(root)),
            tree: Mutex::new(None),
        })
    }

    // A conexão da aba fica livre para navegar enquanto o job copia.
    pub(super) fn fork(&self) -> Self {
        Self {
            client: self.client.clone(),
            root: Mutex::new(None),
            tree: Mutex::new(None),
        }
    }

    async fn share(&self, name: &str) -> Res<MappedMutexGuard<'_, Share>> {
        if name.eq_ignore_ascii_case("IPC$") {
            return Err("SMB: expected a disk share".into());
        }
        let mut slot = self.tree.lock().await;
        if !slot
            .as_ref()
            .is_some_and(|share| same_name(share.name(), name) && !share.connection().is_poisoned())
        {
            *slot = None;
            *slot = Some(self.client.connect_share(name).await.map_err(err)?);
        }
        Ok(MutexGuard::map(slot, |slot| slot.as_mut().unwrap()))
    }

    pub(super) async fn list(&self, path: &str) -> Res<Vec<Entry>> {
        let path = clean(path)?;
        if path == "/" {
            let mut root = self.root.lock().await;
            let mut client = match root.take() {
                Some(client) => client,
                None => self.client.connect_srvsvc().await.map_err(err)?,
            };
            let shares = client.share_enum_level1().await.map_err(err)?;
            *root = Some(client);
            return Ok(shares
                .into_iter()
                .filter(|s| s.share_type & 0x80ff_ffff == 0 && component(&s.name))
                .map(|s| Entry {
                    path: join("/", &s.name),
                    name: s.name,
                    dir: true,
                    link: false,
                    size: 0,
                    mtime: None,
                })
                .collect());
        }
        let (share, relative) = split(&path);
        children(&mut *self.share(share).await?, &path, relative, true).await
    }

    pub(super) async fn existing(&self, dir: &str, names: Vec<String>) -> Res<Vec<String>> {
        let dir = clean(dir)?;
        if dir == "/" {
            let shares = self.list(&dir).await?;
            return Ok(names
                .into_iter()
                .filter(|name| shares.iter().any(|e| same_name(&e.name, name)))
                .collect());
        }
        let (share, relative) = split(&dir);
        let mut share = self.share(share).await?;
        let mut found = Vec::new();
        for name in names.into_iter().filter(|name| component(name)) {
            let path = join(relative, &name);
            let args = request(
                &path,
                READ_ATTRIBUTES,
                CreateDisposition::Open,
                OPEN_REPARSE_POINT,
            );
            match open_item(&mut share, &args).await {
                Ok(open) => {
                    close(&mut share, open.file_id).await?;
                    found.push(name);
                }
                Err(CoreError::UnexpectedStatus { status, .. })
                    if [
                        NtStatus::OBJECT_NAME_NOT_FOUND.0,
                        NtStatus::OBJECT_PATH_NOT_FOUND.0,
                        0xc000_000f, // STATUS_NO_SUCH_FILE
                    ]
                    .contains(&status) => {}
                Err(e) => return Err(err(e)),
            }
        }
        Ok(found)
    }

    pub(super) async fn mkdir(&self, path: &str, merge: bool) -> Res<()> {
        let (name, path) = target(path)?;
        let mut share = self.share(name).await?;
        verify_parent(&mut share, path).await?;
        let mut args = request(
            path,
            READ_ATTRIBUTES,
            if merge {
                CreateDisposition::OpenIf
            } else {
                CreateDisposition::Create
            },
            CreateOptions::DIRECTORY_FILE | OPEN_REPARSE_POINT,
        );
        args.file_attributes = FileAttributes::DIRECTORY;
        let open = open_checked(&mut share, &args).await?;
        let result = async {
            if merge {
                verify_opened_name(&mut share, open.file_id, path).await?;
            }
            Ok(())
        }
        .await;
        result.and(close(&mut share, open.file_id).await)
    }

    pub(super) async fn rename(&self, from: &str, to: &str) -> Res<()> {
        let (name, from) = target(from)?;
        let (to_share, to) = target(to)?;
        if !same_name(name, to_share) {
            return Err("SMB: cannot rename across shares".into());
        }
        let mut share = self.share(name).await?;
        verify_parent(&mut share, from).await?;
        verify_parent(&mut share, to).await?;
        let args = request(from, DELETE, CreateDisposition::Open, OPEN_REPARSE_POINT);
        let open = open_item(&mut share, &args).await.map_err(err)?;
        let result = async {
            if not_link(&mut share, &open).await {
                verify_opened_name(&mut share, open.file_id, from).await?;
            }
            share
                .connection_mut()
                .set_info(&SetInfoRequest::for_file_info(
                    open.file_id,
                    FileInfoClass::RenameInformation,
                    RenameInformation::from_path(&to.replace('/', "\\"), false).encode(),
                ))
                .await
                .map(|_| ())
                .map_err(err)
        }
        .await;
        result.and(close(&mut share, open.file_id).await)
    }

    pub(super) async fn remove(&self, path: &str) -> Res<()> {
        let (name, relative) = target(path)?;
        let mut share = self.share(name).await?;
        verify_parent(&mut share, relative).await?;
        remove_tree(&mut share, relative, 0).await
    }

    pub(super) async fn download_size(&self, path: &str, dir: bool) -> Res<u64> {
        let path = clean(path)?;
        let (name, relative) = split(&path);
        if name.is_empty() {
            return Err("SMB: select a disk share or item to download".into());
        }
        let mut share = self.share(name).await?;
        let args = request(
            relative,
            READ_ATTRIBUTES,
            CreateDisposition::Open,
            OPEN_REPARSE_POINT,
        );
        let open = open_checked(&mut share, &args).await?;
        close(&mut share, open.file_id).await?;
        if open.file_attributes.contains(FileAttributes::DIRECTORY) != dir {
            return Err("SMB: item type changed".into());
        }
        Ok(open.end_of_file)
    }

    // Tags an existing file like a Windows filter driver would (FSCTL_SET_REPARSE_POINT).
    #[cfg(test)]
    pub(super) async fn set_reparse_tag(&self, path: &str, tag: u32) -> Res<()> {
        use smolder_proto::smb::smb2::{CtlCode, IoctlRequest};
        let (name, path) = target(path)?;
        let mut share = self.share(name).await?;
        let args = request(
            path,
            READ_ATTRIBUTES | WRITE_DATA | 0x100, // FILE_WRITE_ATTRIBUTES
            CreateDisposition::Open,
            OPEN_REPARSE_POINT,
        );
        let open = open_item(&mut share, &args).await.map_err(err)?;
        let data = b"sshdeck!";
        let mut input = tag.to_le_bytes().to_vec();
        input.extend((data.len() as u16).to_le_bytes());
        input.extend([0, 0]);
        input.extend(data);
        let set = share
            .connection_mut()
            .ioctl(&IoctlRequest::fsctl(
                CtlCode(0x0009_00a4), // FSCTL_SET_REPARSE_POINT
                open.file_id,
                0,
                input,
            ))
            .await
            .map(|_| ())
            .map_err(err);
        set.and(close(&mut share, open.file_id).await)
    }

    pub(super) async fn copy_file(
        &self,
        step: &Step,
        upload: bool,
        prog: &mut Progress<'_>,
    ) -> Res<()> {
        let (name, path) = target(if upload { &step.dst } else { &step.src })?;
        let mut share = self.share(name).await?;
        if upload {
            verify_parent(&mut share, path).await?;
        }
        let mut args = request(
            path,
            READ_ATTRIBUTES | if upload { WRITE_DATA } else { READ_DATA },
            if upload {
                CreateDisposition::Create
            } else {
                CreateDisposition::Open
            },
            CreateOptions::NON_DIRECTORY_FILE | OPEN_REPARSE_POINT,
        );
        let source = if upload {
            Some(tokio::fs::File::open(&step.src).await.map_err(err)?)
        } else {
            None
        };
        check_cancel(prog.cancel)?;
        let (open, created) = match open_item(&mut share, &args).await {
            Ok(open) => (open, upload),
            Err(CoreError::UnexpectedStatus {
                command: Command::Create,
                status,
            }) if upload && status == NtStatus::OBJECT_NAME_COLLISION.0 => {
                args.create_disposition = CreateDisposition::Open;
                (open_item(&mut share, &args).await.map_err(err)?, false)
            }
            Err(e) => return Err(err(e)),
        };
        let open = settle(&mut share, open, &args).await?;
        let id = open.file_id;
        let result = async {
            if upload {
                // FILE_CREATE on a verified parent cannot open or truncate an
                // existing target. Overwrites still require proof of the name.
                if !opened_name_matches(&mut share, id, path).await? && !created {
                    return Err(UNVERIFIED_PATH.into());
                }
            } else {
                verify_read_path(&mut share, id, path).await?;
            }
            check_cancel(prog.cancel)?;
            if let Some(mut local) = source {
                if open.end_of_file != 0 {
                    truncate(&mut share, id).await?;
                }
                let mut buf = vec![0; BLOCK];
                let mut offset = 0;
                loop {
                    check_cancel(prog.cancel)?;
                    let n = local.read(&mut buf).await.map_err(err)?;
                    if n == 0 {
                        break;
                    }
                    let mut pending = &buf[..n];
                    while !pending.is_empty() {
                        check_cancel(prog.cancel)?;
                        let written = share
                            .connection_mut()
                            .write(&WriteRequest::for_file(id, offset, pending.to_vec()))
                            .await
                            .map_err(err)?
                            .count as usize;
                        if written == 0 || written > pending.len() {
                            return Err("SMB: invalid write size".into());
                        }
                        pending = &pending[written..];
                        offset += written as u64;
                        prog.add(written as u64);
                    }
                }
                share
                    .connection_mut()
                    .flush(&FlushRequest::for_file(id))
                    .await
                    .map_err(err)?;
                Ok(())
            } else {
                let mut local = tokio::fs::File::create(&step.dst).await.map_err(err)?;
                let mut offset = 0;
                while offset < open.end_of_file {
                    check_cancel(prog.cancel)?;
                    let want = (open.end_of_file - offset).min(BLOCK as u64) as u32;
                    let data = share
                        .connection_mut()
                        .read(&ReadRequest::for_file(id, offset, want))
                        .await
                        .map_err(err)?
                        .data;
                    if data.is_empty() || data.len() > want as usize {
                        return Err("SMB: invalid read size or unexpected end of file".into());
                    }
                    local.write_all(&data).await.map_err(err)?;
                    offset += data.len() as u64;
                    prog.add(data.len() as u64);
                }
                local.flush().await.map_err(err)
            }
        }
        .await;
        result.and(close(&mut share, id).await)
    }
}

async fn children(
    share: &mut Share,
    dir: &str,
    relative: &str,
    read_only: bool,
) -> Res<Vec<Entry>> {
    let args = request(
        relative,
        READ_DATA | READ_ATTRIBUTES,
        CreateDisposition::Open,
        CreateOptions::DIRECTORY_FILE | OPEN_REPARSE_POINT,
    );
    let open = open_checked(share, &args).await?;
    let result: Res<Vec<Entry>> = async {
        if !relative.is_empty() {
            if read_only {
                verify_read_path(share, open.file_id, relative).await?;
            } else {
                verify_opened_name(share, open.file_id, relative).await?;
            }
        }
        let limits = share.connection().resource_limits();
        let mut query = QueryDirectoryRequest::for_pattern(open.file_id, "*", BLOCK as u32);
        let mut entries = Vec::new();
        let mut batch = PAGES_PER_TRIP;
        let mut pages = 0;
        while pages < limits.max_directory_pages {
            let answers = match query_directory_pages(share, &query, batch).await {
                Ok(answers) => answers,
                // Fewer credits than pages, or a server that rejects compounded
                // enumeration: start over one page per round trip.
                Err(_) if batch > 1 && !share.connection().is_poisoned() => {
                    batch = 1;
                    entries.clear();
                    query.flags = QueryDirectoryFlags::RESTART_SCANS;
                    continue;
                }
                Err(e) => return Err(err(e)),
            };
            for page in answers {
                pages += 1;
                let Some(page) = page else {
                    return Ok(entries);
                };
                decode_directory_page(&page, dir, &mut entries)?;
                if entries.len() > limits.max_directory_entries {
                    return Err("SMB: directory has too many entries".into());
                }
            }
            // Samba requires a buffer byte even on the continuation request.
            query.flags = QueryDirectoryFlags::empty();
        }
        Err("SMB: directory has too many pages".into())
    }
    .await;
    let closed = close(share, open.file_id).await;
    let entries = result?;
    closed?;
    Ok(entries)
}

// `count` compounded QUERY_DIRECTORY requests for FileFullDirectoryInformation (which the
// crate's request type can't name) in one round trip; each continues the enumeration where
// the previous one stopped. None marks STATUS_NO_MORE_FILES.
async fn query_directory_pages(
    share: &mut Share,
    query: &QueryDirectoryRequest,
    count: usize,
) -> Result<Vec<Option<Vec<u8>>>, CoreError> {
    let mut query = query.clone();
    let mut requests = Vec::with_capacity(count);
    for _ in 0..count {
        let mut body = query.encode();
        body[2] = FULL_DIRECTORY;
        let mut request = CompoundRequest::new(Command::QueryDirectory, body);
        request.accepted_statuses = vec![NtStatus::SUCCESS.0, NtStatus::NO_MORE_FILES.0];
        requests.push(request);
        query.flags = QueryDirectoryFlags::empty();
    }
    let mut pages = Vec::with_capacity(count);
    for packet in share.connection_mut().compound_raw(&requests).await? {
        if packet.header.status == NtStatus::NO_MORE_FILES.0 {
            pages.push(None);
            break;
        }
        let page = QueryDirectoryResponse::decode(&packet.body)?.output_buffer;
        pages.push((!page.is_empty()).then_some(page));
    }
    Ok(pages)
}

// FILE_FULL_DIR_INFORMATION: 68-byte fixed header followed by a UTF-16LE name. EaSize
// holds the reparse tag when FILE_ATTRIBUTE_REPARSE_POINT is set (MS-FSCC 2.4.14).
// Unlike the crate's convenience decoder, do not reject valid Windows attribute bits
// that its FileAttributes enum doesn't define (e.g. reparse or compressed files).
fn decode_directory_page(mut data: &[u8], dir: &str, entries: &mut Vec<Entry>) -> Res<()> {
    while !data.is_empty() {
        let bad = || "SMB: invalid directory response".to_owned();
        let header = data.get(..68).ok_or_else(bad)?;
        let next = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let entry = if next == 0 {
            data
        } else {
            data.get(..next).ok_or_else(bad)?
        };
        let attrs = FileAttributes::from_bits_retain(u32::from_le_bytes(
            header[56..60].try_into().unwrap(),
        ));
        let len = u32::from_le_bytes(header[60..64].try_into().unwrap()) as usize;
        let name = utf16le(
            entry
                .get(68..)
                .and_then(|tail| tail.get(..len))
                .ok_or_else(bad)?,
        )?;
        if component(&name) {
            let dir_entry = attrs.contains(FileAttributes::DIRECTORY);
            let filetime = u64::from_le_bytes(header[24..32].try_into().unwrap());
            entries.push(Entry {
                path: join(dir, &name),
                name,
                dir: dir_entry,
                link: attrs.contains(REPARSE_POINT)
                    && link_tag(u32::from_le_bytes(header[64..68].try_into().unwrap())),
                size: if dir_entry {
                    0
                } else {
                    u64::from_le_bytes(header[40..48].try_into().unwrap())
                },
                mtime: (filetime / 10_000_000).checked_sub(11_644_473_600),
            });
        }
        if next == 0 {
            break;
        }
        data = &data[next..];
    }
    Ok(())
}

async fn remove_tree(share: &mut Share, path: &str, depth: usize) -> Res<()> {
    let args = request(
        path,
        DELETE | READ_ATTRIBUTES,
        CreateDisposition::Open,
        OPEN_REPARSE_POINT,
    );
    let open = open_item(share, &args).await.map_err(err)?;
    let result = async {
        let item = not_link(share, &open).await;
        if item {
            verify_opened_name(share, open.file_id, path).await?;
        }
        if open.file_attributes.contains(FileAttributes::DIRECTORY) && item {
            if depth >= MAX_DEPTH {
                return Err("SMB: directory is nested too deeply".into());
            }
            for child in children(share, path, path, false).await? {
                Box::pin(remove_tree(share, &child.path, depth + 1)).await?;
            }
        }
        share
            .connection_mut()
            .set_info(&SetInfoRequest::for_file_info(
                open.file_id,
                FileInfoClass::DispositionInformation,
                DispositionInformation {
                    delete_pending: true,
                }
                .encode(),
            ))
            .await
            .map(|_| ())
            .map_err(err)
    }
    .await;
    result.and(close(share, open.file_id).await)
}

fn request(
    path: &str,
    access: u32,
    disposition: CreateDisposition,
    options: CreateOptions,
) -> CreateRequest {
    let mut args = CreateRequest::from_path(&path.replace('/', "\\"));
    args.desired_access = access;
    args.share_access = ShareAccess::READ | ShareAccess::WRITE | ShareAccess::DELETE;
    args.create_disposition = disposition;
    args.create_options = options;
    args
}

async fn open_item(
    share: &mut Share,
    request: &CreateRequest,
) -> Result<CreateResponse, CoreError> {
    let mut packets = share
        .connection_mut()
        .compound_raw(&[CompoundRequest::new(Command::Create, request.encode())])
        .await?;
    let mut packet = packets
        .pop()
        .ok_or(CoreError::InvalidResponse("missing SMB create response"))?;
    // Preserve attributes the low-level crate doesn't recognize after its other decoding checks.
    let bytes = packet
        .body
        .get_mut(56..60)
        .ok_or(CoreError::InvalidResponse("truncated SMB create response"))?;
    let attrs = u32::from_le_bytes(bytes.try_into().unwrap());
    bytes.copy_from_slice(
        &FileAttributes::from_bits_truncate(attrs)
            .bits()
            .to_le_bytes(),
    );
    let mut response = CreateResponse::decode(&packet.body)?;
    response.file_attributes = FileAttributes::from_bits_retain(attrs);
    Ok(response)
}

fn unsupported_name_query(error: &CoreError) -> bool {
    matches!(error, CoreError::UnexpectedStatus { command: Command::QueryInfo, status }
        if [0xc000_0003, 0xc000_000d, 0xc000_00a4, 0xc000_00bb].contains(status))
}

// Some servers support neither name class over SMB. Unsupported is distinct
// from an actual mismatch, malformed response, access failure or transport error.
async fn opened_name_matches(share: &mut Share, id: FileId, expected: &str) -> Res<bool> {
    let answer = match query_info_raw(share, id, NORMALIZED_NAME).await {
        Err(e) if unsupported_name_query(&e) => query_info_raw(share, id, FILE_NAME).await,
        result => result,
    };
    let data = match answer {
        Err(e) if unsupported_name_query(&e) => return Ok(false),
        result => result.map_err(err)?,
    };
    let len = u32::from_le_bytes(
        data.get(..4)
            .ok_or("SMB: invalid file name response")?
            .try_into()
            .unwrap(),
    ) as usize;
    let name = utf16le(
        data.get(4..)
            .and_then(|tail| tail.get(..len))
            .ok_or("SMB: invalid file name response")?,
    )?;
    let expected = expected.replace('/', "\\");
    if !same_name(name.trim_start_matches('\\'), &expected) {
        return Err("SMB: refusing to operate on a path redirected by the server".into());
    }
    Ok(true)
}

async fn verify_opened_name(share: &mut Share, id: FileId, path: &str) -> Res<()> {
    if !opened_name_matches(share, id, path).await? {
        // Samba can follow UNIX symlinks without reporting a reparse point.
        return Err(UNVERIFIED_PATH.into());
    }
    Ok(())
}

// The caller already rejected a reparse point on the final open. Check each
// ancestor separately for Windows-style reparse points. This cannot detect a
// transparent UNIX symlink, so never use this fallback to authorize writes.
async fn verify_read_path(share: &mut Share, id: FileId, path: &str) -> Res<()> {
    if !opened_name_matches(share, id, path).await? {
        for (end, _) in path.match_indices('/') {
            let args = request(
                &path[..end],
                READ_ATTRIBUTES,
                CreateDisposition::Open,
                CreateOptions::DIRECTORY_FILE | OPEN_REPARSE_POINT,
            );
            let open = open_checked(share, &args).await?;
            close(share, open.file_id).await?;
        }
    }
    Ok(())
}

async fn query_info_raw(
    share: &mut Share,
    id: FileId,
    info_class: u8,
) -> Result<Vec<u8>, CoreError> {
    #[cfg(test)]
    if LEGACY_NAME_STATUS.try_with(|()| ()).is_ok()
        && matches!(info_class, NORMALIZED_NAME | FILE_NAME)
    {
        return Err(CoreError::UnexpectedStatus {
            command: Command::QueryInfo,
            status: 0xc000_00bb,
        });
    }
    let mut request = QueryInfoRequest::for_file_info(id, FileInfoClass::BasicInformation);
    request.output_buffer_length = BLOCK as u32;
    let mut body = request.encode();
    body[3] = info_class;
    let packets = share
        .connection_mut()
        .compound_raw(&[CompoundRequest::new(Command::QueryInfo, body)])
        .await?;
    let packet = packets.first().ok_or(CoreError::InvalidResponse(
        "missing SMB query info response",
    ))?;
    Ok(QueryInfoResponse::decode(&packet.body)?.output_buffer)
}

async fn verify_parent(share: &mut Share, path: &str) -> Res<()> {
    let Some((parent, _)) = path.rsplit_once('/') else {
        return Ok(());
    };
    let args = request(
        parent,
        READ_ATTRIBUTES,
        CreateDisposition::Open,
        CreateOptions::DIRECTORY_FILE | OPEN_REPARSE_POINT,
    );
    let open = open_checked(share, &args).await?;
    let result = verify_opened_name(share, open.file_id, parent).await;
    result.and(close(share, open.file_id).await)
}

async fn truncate(share: &mut Share, id: FileId) -> Res<()> {
    // The crate's FileInfoClass doesn't include FileEndOfFileInformation yet.
    let mut body = SetInfoRequest::for_file_info(
        id,
        FileInfoClass::BasicInformation,
        0_u64.to_le_bytes().to_vec(),
    )
    .encode();
    body[3] = 0x14; // FileEndOfFileInformation
    share
        .connection_mut()
        .compound_raw(&[CompoundRequest::new(Command::SetInfo, body)])
        .await
        .map(|_| ())
        .map_err(err)
}

fn utf16le(data: &[u8]) -> Res<String> {
    let (pairs, rest) = data.as_chunks::<2>();
    if !rest.is_empty() {
        return Err("SMB: invalid UTF-16 file name".into());
    }
    let units: Vec<_> = pairs.iter().copied().map(u16::from_le_bytes).collect();
    String::from_utf16(&units).map_err(err)
}

async fn close(share: &mut Share, file_id: FileId) -> Res<()> {
    let close = CloseRequest { flags: 0, file_id };
    share
        .connection_mut()
        .compound_raw(&[CompoundRequest::new(Command::Close, close.encode())])
        .await
        .map(|_| ())
        .map_err(err)
}

// Name surrogates (symlinks, junctions, WSL links) point at another item; NFS-style and WSL
// special files aren't regular data either. Deduplicated, cloud (OneDrive/Azure File Sync),
// WOF-compressed and HSM files carry their own data behind a filter driver.
fn link_tag(tag: u32) -> bool {
    const NAME_SURROGATE: u32 = 0x2000_0000;
    tag == 0
        || tag & NAME_SURROGATE != 0
        || matches!(
            tag,
            0x8000_0014 // IO_REPARSE_TAG_NFS
            | 0x8000_0023 // IO_REPARSE_TAG_AF_UNIX
            | 0x8000_0024 // IO_REPARSE_TAG_LX_FIFO
            | 0x8000_0025 // IO_REPARSE_TAG_LX_CHR
            | 0x8000_0026 // IO_REPARSE_TAG_LX_BLK
        )
}

// Some(tag) when the handle (opened with FILE_OPEN_REPARSE_POINT) is a data reparse point
// that must be reopened through its filter. A server unable to report the tag gets no trust.
async fn data_reparse(share: &mut Share, open: &CreateResponse) -> Res<Option<u32>> {
    if !open.file_attributes.contains(REPARSE_POINT) {
        return Ok(None);
    }
    let data = query_info_raw(share, open.file_id, ATTRIBUTE_TAG)
        .await
        .map_err(|_| REPARSE_LINK.to_owned())?;
    let tag = data
        .get(4..8)
        .ok_or("SMB: invalid attribute tag response")?;
    let tag = u32::from_le_bytes(tag.try_into().unwrap());
    if link_tag(tag) {
        return Err(REPARSE_LINK.into());
    }
    Ok(Some(tag))
}

// Accepts an item opened with FILE_OPEN_REPARSE_POINT. Links are refused; data reparse
// points are reopened without the flag so the server's filter supplies the real data and
// directory contents. The caller still verifies the reopened handle's name. On error the
// handle is closed.
async fn settle(
    share: &mut Share,
    open: CreateResponse,
    args: &CreateRequest,
) -> Res<CreateResponse> {
    let tag = match data_reparse(share, &open).await {
        Ok(None) => return Ok(open),
        Ok(Some(tag)) => {
            close(share, open.file_id).await?;
            tag
        }
        Err(e) => {
            let _ = close(share, open.file_id).await;
            return Err(e);
        }
    };
    let mut args = args.clone();
    args.create_disposition = CreateDisposition::Open;
    args.create_options =
        CreateOptions::from_bits_retain(args.create_options.bits() & !OPEN_REPARSE_POINT.bits());
    let reopened = open_item(share, &args).await.map_err(|e| match e {
        CoreError::UnexpectedStatus {
            status: 0xc000_0279,
            ..
        } => format!(
            "SMB: the server has no filter for reparse tag {tag:#x} and can't read this item"
        ),
        e => err(e),
    })?;
    if let Err(e) = data_reparse(share, &reopened).await {
        let _ = close(share, reopened.file_id).await;
        return Err(e);
    }
    Ok(reopened)
}

async fn open_checked(share: &mut Share, args: &CreateRequest) -> Res<CreateResponse> {
    let open = open_item(share, args).await.map_err(err)?;
    settle(share, open, args).await
}

// Whether to treat an item opened with FILE_OPEN_REPARSE_POINT as itself (not a link).
async fn not_link(share: &mut Share, open: &CreateResponse) -> bool {
    data_reparse(share, open).await.is_ok()
}

fn same_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
}

fn component(name: &str) -> bool {
    !matches!(name, "" | "." | "..") && !name.contains(['/', '\\', ':', '\0'])
}

pub(super) fn clean(path: &str) -> Res<String> {
    if path.contains(['\\', ':', '\0']) {
        return Err("SMB: invalid path".into());
    }
    Ok(normalize(path))
}

fn split(path: &str) -> (&str, &str) {
    let path = path.trim_start_matches('/');
    path.split_once('/').unwrap_or((path, ""))
}

fn target(path: &str) -> Res<(&str, &str)> {
    let (share, relative) = split(path);
    if !component(share) || !relative.split('/').all(component) {
        return Err("SMB: select an item inside a disk share".into());
    }
    Ok((share, relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    // One FILE_FULL_DIR_INFORMATION record (MS-FSCC 2.4.14).
    fn full_dir_record(name: &str, attrs: u32, ea_or_tag: u32, last: bool) -> Vec<u8> {
        let name: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let len = (68 + name.len()).next_multiple_of(8);
        let mut out = vec![0; len];
        if !last {
            out[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        }
        out[40..48].copy_from_slice(&7_u64.to_le_bytes());
        out[56..60].copy_from_slice(&attrs.to_le_bytes());
        out[60..64].copy_from_slice(&(name.len() as u32).to_le_bytes());
        out[64..68].copy_from_slice(&ea_or_tag.to_le_bytes());
        out[68..68 + name.len()].copy_from_slice(&name);
        out
    }

    #[test]
    fn only_name_surrogate_and_special_reparse_tags_are_links() {
        for tag in [
            0xa000_000c, // IO_REPARSE_TAG_SYMLINK
            0xa000_0003, // IO_REPARSE_TAG_MOUNT_POINT
            0xa000_001d, // IO_REPARSE_TAG_LX_SYMLINK
            0x8000_0014, // IO_REPARSE_TAG_NFS (Samba/NFS symlinks and devices)
            0x8000_0023, // IO_REPARSE_TAG_AF_UNIX
            0x8000_0024, // IO_REPARSE_TAG_LX_FIFO
            0x8000_0025, // IO_REPARSE_TAG_LX_CHR
            0x8000_0026, // IO_REPARSE_TAG_LX_BLK
            0,           // unknown: stay conservative
        ] {
            assert!(link_tag(tag), "{tag:#x}");
        }
        for tag in [
            0x8000_0013, // IO_REPARSE_TAG_DEDUP
            0x8000_0017, // IO_REPARSE_TAG_WOF
            0x9000_001a, // IO_REPARSE_TAG_CLOUD
            0x9000_601a, // IO_REPARSE_TAG_CLOUD_6
            0x8000_001b, // IO_REPARSE_TAG_APPEXECLINK is not a name surrogate either
            0x8000_000a, // IO_REPARSE_TAG_DFSR
            0xc000_0004, // IO_REPARSE_TAG_HSM
        ] {
            assert!(!link_tag(tag), "{tag:#x}");
        }
    }

    #[test]
    fn directory_page_reads_the_reparse_tag_of_each_entry() {
        let reparse = REPARSE_POINT.bits() | FileAttributes::ARCHIVE.bits();
        let mut page = full_dir_record("plain.png", FileAttributes::ARCHIVE.bits(), 0, false);
        page.extend(full_dir_record("dedup.png", reparse, 0x8000_0013, false));
        page.extend(full_dir_record("cloud", reparse | 0x10, 0x9000_601a, false));
        page.extend(full_dir_record("link", reparse, 0xa000_000c, true));
        let mut entries = Vec::new();
        decode_directory_page(&page, "/s/d", &mut entries).unwrap();
        let got: Vec<_> = entries
            .iter()
            .map(|e| (e.path.as_str(), e.dir, e.link, e.size))
            .collect();
        assert_eq!(
            got,
            [
                ("/s/d/plain.png", false, false, 7),
                ("/s/d/dedup.png", false, false, 7),
                ("/s/d/cloud", true, false, 0),
                ("/s/d/link", false, true, 7),
            ]
        );
    }
}
