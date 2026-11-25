//! Área de transferência compartilhada no RDP (canal CLIPRDR), só texto e só na aba em foco.

use crate::clip;
use ironrdp_cliprdr::{
    backend::{ClipboardMessage, CliprdrBackend},
    pdu::{
        ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
        FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
        OwnedFormatDataResponse,
    },
};
use ironrdp_core::{IntoOwned, impl_as_any};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Debug, Default)]
struct Owner {
    id: Option<u32>,
    revision: u64,
}

/// Uma única dona do clipboard RDP. A revisão também invalida operações pendentes ao voltar à aba.
#[derive(Clone, Debug, Default)]
pub struct Focus(Arc<Mutex<Owner>>);

impl Focus {
    pub fn update(&self, id: Option<u32>, revision: u64) -> bool {
        let mut owner = self.0.lock().unwrap();
        if revision <= owner.revision {
            return false; // uma chamada IPC antiga chegou depois da nova
        }
        *owner = Owner { id, revision };
        true
    }

    pub fn close(&self, id: u32) {
        let mut owner = self.0.lock().unwrap();
        if owner.id == Some(id) {
            owner.id = None;
        }
    }

    fn revision(&self, id: u32) -> Option<u64> {
        let owner = self.0.lock().unwrap();
        (owner.id == Some(id)).then_some(owner.revision)
    }

    fn with_owner<T>(&self, id: u32, revision: u64, f: impl FnOnce() -> T) -> Option<T> {
        // A troca de aba não pode ocorrer entre a checagem e a operação no clipboard.
        let owner = self.0.lock().unwrap();
        (owner.id == Some(id) && owner.revision == revision).then(f)
    }
}

pub enum Event {
    MonitorReady,
    Ready,
    RemoteCopy(Option<(u64, ClipboardFormatId)>),
    DataRequest(Option<u64>, ClipboardFormatId),
    DataResponse(OwnedFormatDataResponse),
}

#[derive(Debug)]
pub struct Backend {
    tx: mpsc::UnboundedSender<Event>,
    focus: Focus,
    id: u32,
}

impl Backend {
    pub fn new(tx: mpsc::UnboundedSender<Event>, focus: Focus, id: u32) -> Self {
        Self { tx, focus, id }
    }

    fn send(&self, event: Event) {
        let _ = self.tx.send(event);
    }
}

impl_as_any!(Backend);

impl CliprdrBackend for Backend {
    fn temporary_directory(&self) -> &str {
        ""
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }

    fn on_ready(&mut self) {
        self.send(Event::Ready);
    }

    fn on_request_format_list(&mut self) {
        self.send(Event::MonitorReady);
    }

    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        let offer = self.focus.revision(self.id).and_then(|revision| {
            [
                ClipboardFormatId::CF_UNICODETEXT,
                ClipboardFormatId::CF_TEXT,
            ]
            .into_iter()
            .find(|id| formats.iter().any(|format| format.id == *id))
            .map(|id| (revision, id))
        });
        self.send(Event::RemoteCopy(offer));
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.send(Event::DataRequest(
            self.focus.revision(self.id),
            request.format,
        ));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        self.send(Event::DataResponse(response.into_owned()));
    }

    fn on_file_contents_request(&mut self, _: FileContentsRequest) {}
    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}
    fn on_lock(&mut self, _: LockDataId) {}
    fn on_unlock(&mut self, _: LockDataId) {}
}

pub struct Session {
    focus: Focus,
    id: u32,
    sent_initial: bool,
    ready: bool,
    /// Só guarda a impressão do texto local efetivamente anunciado, nunca texto recebido.
    advertised: Option<(u64, [u8; 32])>,
    offer: Option<(u64, ClipboardFormatId)>,
    /// Respostas não têm ID no protocolo: só um pedido por vez, inclusive ao trocar de aba.
    pending: Option<(u64, ClipboardFormatId)>,
    superseded: bool,
}

impl Session {
    pub fn new(focus: Focus, id: u32) -> Self {
        Self {
            focus,
            id,
            sent_initial: false,
            ready: false,
            advertised: None,
            offer: None,
            pending: None,
            superseded: false,
        }
    }

    fn advertise(&mut self, read: impl FnOnce() -> Option<String>) -> Option<Vec<ClipboardFormat>> {
        let snapshot = self.focus.revision(self.id).and_then(|revision| {
            self.focus
                .with_owner(self.id, revision, read)
                .flatten()
                .filter(|text| !text.is_empty() && !clip::is_secret(text))
                .map(|text| (revision, clip::hash(&text)))
        });
        let changed = snapshot != self.advertised;
        self.advertised = snapshot;
        (changed && snapshot.is_some())
            .then(|| vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
    }

    pub fn poll(&mut self, read: impl FnOnce() -> Option<String>) -> Option<ClipboardMessage> {
        if !self.ready {
            return None;
        }
        self.advertise(read).map(ClipboardMessage::SendInitiateCopy)
    }

    pub fn handle(
        &mut self,
        event: Event,
        read: impl FnOnce() -> Option<String>,
        write: impl FnOnce(&str) -> Option<()>,
    ) -> Option<ClipboardMessage> {
        use ClipboardMessage as M;
        match event {
            Event::MonitorReady if !self.sent_initial => {
                // No IronRDP 0.7, esta lista (mesmo vazia) inicia a negociação. Só o aceite
                // dela chama on_ready; antecipar initiate_copy repetiria Capabilities + TempDir.
                self.sent_initial = true;
                Some(M::SendInitiateCopy(
                    self.advertise(read).unwrap_or_default(),
                ))
            }
            Event::MonitorReady => None,
            Event::Ready => {
                self.ready = true;
                self.request_offer().or_else(|| self.poll(read))
            }
            Event::RemoteCopy(offer) => {
                self.offer = offer;
                self.superseded = self.pending.is_some();
                self.request_offer()
            }
            Event::DataRequest(revision, format) if self.ready => {
                Some(M::SendFormatData(self.respond(revision, format, read)))
            }
            Event::DataRequest(..) => None, // o IronRDP ainda não aceita submit_format_data
            Event::DataResponse(response) => {
                if let Some((revision, format)) = self.pending.take()
                    && !self.superseded
                    && !response.is_error()
                    && self
                        .focus
                        .with_owner(self.id, revision, || {
                            let text = if format == ClipboardFormatId::CF_UNICODETEXT {
                                response.to_unicode_string()
                            } else {
                                response.to_string()
                            }
                            .ok()?;
                            // O Windows termina o texto com \0 e usa CRLF.
                            let text = text.trim_end_matches('\0').replace("\r\n", "\n");
                            (!text.is_empty()).then(|| write(&text)).flatten()
                        })
                        .flatten()
                        .is_some()
                {
                    self.advertised = None;
                }
                self.request_offer()
            }
        }
    }

    fn request_offer(&mut self) -> Option<ClipboardMessage> {
        if !self.ready || self.pending.is_some() {
            return None;
        }
        let (revision, format) = self.offer.take()?;
        if self.focus.revision(self.id) != Some(revision) {
            return None;
        }
        self.pending = Some((revision, format));
        self.superseded = false;
        Some(ClipboardMessage::SendInitiatePaste(format))
    }

    fn respond(
        &mut self,
        revision: Option<u64>,
        format: ClipboardFormatId,
        read: impl FnOnce() -> Option<String>,
    ) -> OwnedFormatDataResponse {
        let mut response = None;
        if let (Some(revision), Some((advertised_revision, digest))) = (revision, self.advertised)
            && revision == advertised_revision
        {
            response = self
                .focus
                .with_owner(self.id, revision, || {
                    let text = read()
                        .filter(|text| !clip::is_secret(text) && clip::hash(text) == digest)?;
                    match format {
                        ClipboardFormatId::CF_UNICODETEXT => {
                            Some(FormatDataResponse::new_unicode_string(&text))
                        }
                        ClipboardFormatId::CF_TEXT => Some(FormatDataResponse::new_string(&text)),
                        _ => Some(FormatDataResponse::new_error()),
                    }
                })
                .flatten();
            if response.is_none() {
                self.advertised = None;
            }
        }
        response
            .unwrap_or_else(FormatDataResponse::new_error)
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_cliprdr::{
        CliprdrClient,
        pdu::{ClipboardPdu, FormatListResponse},
    };
    use ironrdp_core::{decode, encode_vec};
    use ironrdp_svc::{SvcMessage, SvcProcessor};

    const TEXT: ClipboardFormatId = ClipboardFormatId::CF_UNICODETEXT;

    fn never_read() -> Option<String> {
        panic!("a aba não pode ler o clipboard")
    }

    fn never_write(_: &str) -> Option<()> {
        panic!("a aba não pode escrever no clipboard")
    }

    fn active_session() -> (Focus, Session) {
        let focus = Focus::default();
        focus.update(Some(1), 1);
        let mut session = Session::new(focus.clone(), 1);
        session.handle(Event::MonitorReady, || None, never_write);
        session.handle(Event::Ready, || None, never_write);
        (focus, session)
    }

    fn data(message: Option<ClipboardMessage>) -> OwnedFormatDataResponse {
        let Some(ClipboardMessage::SendFormatData(data)) = message else {
            panic!("esperava resposta de clipboard");
        };
        data
    }

    #[test]
    fn pinned_cliprdr_starts_on_monitor_ready_and_polls_only_after_ack() {
        let focus = Focus::default();
        focus.update(Some(1), 1);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut client = CliprdrClient::new(Box::new(Backend::new(tx, focus.clone(), 1)));
        let mut session = Session::new(focus, 1);

        assert!(session.poll(never_read).is_none());
        assert!(rx.try_recv().is_err());
        client
            .process(&encode_vec(&ClipboardPdu::MonitorReady).unwrap())
            .unwrap();
        let Some(ClipboardMessage::SendInitiateCopy(formats)) =
            session.handle(rx.try_recv().unwrap(), || Some("local".into()), never_write)
        else {
            panic!("Monitor Ready deve iniciar a troca de formatos");
        };
        let frames: Vec<SvcMessage> = client.initiate_copy(&formats).unwrap().into();
        let [caps, temp_dir, list] = frames.as_slice() else {
            panic!("esperava os três PDUs de negociação");
        };
        assert!(matches!(
            decode::<ClipboardPdu<'_>>(&caps.encode_unframed_pdu().unwrap()).unwrap(),
            ClipboardPdu::Capabilities(_)
        ));
        assert!(matches!(
            decode::<ClipboardPdu<'_>>(&temp_dir.encode_unframed_pdu().unwrap()).unwrap(),
            ClipboardPdu::TemporaryDirectory(_)
        ));
        assert!(matches!(
            decode::<ClipboardPdu<'_>>(&list.encode_unframed_pdu().unwrap()).unwrap(),
            ClipboardPdu::FormatList(_)
        ));
        assert!(session.poll(never_read).is_none());

        client
            .process(
                &encode_vec(&ClipboardPdu::FormatListResponse(FormatListResponse::Ok)).unwrap(),
            )
            .unwrap();
        assert!(
            session
                .handle(rx.try_recv().unwrap(), || Some("local".into()), never_write)
                .is_none()
        );
        let Some(ClipboardMessage::SendInitiateCopy(formats)) =
            session.poll(|| Some("novo".into()))
        else {
            panic!("o canal inicializado deve anunciar mudanças");
        };
        let frames: Vec<SvcMessage> = client.initiate_copy(&formats).unwrap().into();
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn hidden_session_negotiates_an_empty_list_without_reading() {
        let focus = Focus::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut backend = Backend::new(tx, focus.clone(), 1);
        let mut session = Session::new(focus, 1);
        assert!(
            matches!(session.handle(Event::MonitorReady, never_read, never_write), Some(ClipboardMessage::SendInitiateCopy(formats)) if formats.is_empty())
        );
        assert!(
            session
                .handle(Event::Ready, never_read, never_write)
                .is_none()
        );
        assert!(session.poll(never_read).is_none());
        backend.on_remote_copy(&[ClipboardFormat::new(TEXT)]);
        assert!(
            session
                .handle(rx.try_recv().unwrap(), never_read, never_write)
                .is_none()
        );
        backend.on_format_data_response(FormatDataResponse::new_unicode_string("remoto"));
        assert!(
            session
                .handle(rx.try_recv().unwrap(), never_read, never_write)
                .is_none()
        );
        backend.on_format_data_request(FormatDataRequest { format: TEXT });
        assert!(data(session.handle(rx.try_recv().unwrap(), never_read, never_write)).is_error());
    }

    #[test]
    fn old_clipboard_requests_cannot_use_the_new_focus() {
        let (focus, mut session) = active_session();
        let local = || Some("local".into());
        assert!(session.poll(local).is_some());
        assert!(matches!(
            session.handle(Event::RemoteCopy(Some((1, TEXT))), never_read, never_write),
            Some(ClipboardMessage::SendInitiatePaste(TEXT))
        ));

        focus.update(Some(2), 3);
        assert!(!focus.update(Some(1), 2)); // IPC atrasado não reativa a aba anterior
        assert!(
            data(session.handle(Event::DataRequest(Some(1), TEXT), never_read, never_write))
                .is_error()
        );
        focus.update(Some(1), 4);
        assert!(session.poll(local).is_some());
        assert!(
            session
                .handle(
                    Event::DataResponse(
                        FormatDataResponse::new_unicode_string("remoto").into_owned()
                    ),
                    never_read,
                    never_write
                )
                .is_none()
        );
        assert!(
            data(session.handle(Event::DataRequest(None, TEXT), never_read, never_write))
                .is_error()
        );
        assert_eq!(
            data(session.handle(Event::DataRequest(Some(4), TEXT), local, never_write))
                .to_unicode_string()
                .unwrap(),
            "local"
        );
    }

    #[test]
    fn cached_text_is_not_returned_after_copying_it_as_a_secret() {
        let (_, mut session) = active_session();
        let text = "senha-para-teste-de-cache-rdp";
        let read = || Some(text.into());
        assert!(session.poll(read).is_some());
        clip::publish_secret(text, || ());
        assert!(
            data(session.handle(Event::DataRequest(Some(1), TEXT), read, never_write)).is_error()
        );
        assert!(session.advertised.is_none());
        assert!(session.poll(read).is_none());
    }

    #[test]
    fn empty_or_nontext_clipboard_invalidates_the_advertisement() {
        let (_, mut session) = active_session();
        let local = || Some("local".into());
        for missing in [None, Some(String::new())] {
            assert!(session.poll(local).is_some());
            assert!(session.poll(|| missing).is_none());
            assert!(
                data(session.handle(Event::DataRequest(Some(1), TEXT), never_read, never_write))
                    .is_error()
            );
        }
        // Também rejeita mudança entre duas sondagens, sem enviar o novo texto antes de anunciá-lo.
        assert!(session.poll(local).is_some());
        assert!(
            data(session.handle(
                Event::DataRequest(Some(1), TEXT),
                || Some("novo".into()),
                never_write
            ))
            .is_error()
        );
    }

    #[test]
    fn inbound_text_is_written_but_never_becomes_an_advertisement() {
        let (_, mut session) = active_session();
        assert!(session.poll(|| Some("local".into())).is_some());
        assert!(
            session
                .handle(Event::RemoteCopy(Some((1, TEXT))), never_read, never_write)
                .is_some()
        );
        let inbound = FormatDataResponse::new_unicode_string("remoto\r\ntexto").into_owned();
        let mut written = None;
        session.handle(Event::DataResponse(inbound), never_read, |text| {
            written = Some(text.to_owned());
            Some(())
        });
        assert_eq!(written.as_deref(), Some("remoto\ntexto"));
        assert!(
            data(session.handle(Event::DataRequest(Some(1), TEXT), never_read, never_write))
                .is_error()
        );
    }
}
