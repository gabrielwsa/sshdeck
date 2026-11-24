//! RDP dentro do app, com o IronRDP: a sessão roda numa thread com um runtime tokio próprio,
//! decodifica a tela num buffer RGBA e manda para a janela só os retângulos que mudaram.
//! A janela desenha num `<canvas>` e devolve mouse e teclado (scancodes) pelo `send`.
//!
//! Mensagens binárias para a janela (u16 em little-endian):
//! - `1 x y w h` + w*h*4 bytes RGBA: pedaço da tela
//! - `2 w h`: tamanho da área de trabalho (a janela redimensiona o canvas)
//! - `3 w h hx hy` + w*h*4 bytes RGBA: cursor do Windows
//! - `4`: cursor padrão · `5`: cursor escondido

use anyhow::{Context, Result, anyhow, bail};
use ironrdp_connector::{
    ConnectionResult, ConnectorResult, Credentials, DesktopSize, ServerName,
    connection_activation::ConnectionActivationState,
};
use ironrdp_core::WriteBuf;
use ironrdp_displaycontrol::{client::DisplayControlClient, pdu::MonitorLayoutEntry};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::{
    geometry::InclusiveRectangle,
    input::{
        MousePdu,
        fast_path::{FastPathInputEvent, KeyboardFlags, SynchronizeFlags},
        mouse::PointerFlags,
    },
    rdp::{capability_sets::MajorPlatformType, client_info::PerformanceFlags},
};
use ironrdp_session::{
    ActiveStage, ActiveStageBuilder, ActiveStageOutput, fast_path, image::DecodedImage,
};
use ironrdp_tokio::{FramedWrite, TokioFramed, single_sequence_step_read, split_tokio_framed};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sshdeck_core::{rdp as core_rdp, vault::Host};
use std::{
    net::SocketAddr,
    process::Child,
    thread,
    time::{Duration, Instant},
};
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::{net::TcpStream, sync::mpsc};
use zeroize::Zeroizing;

/// O que a janela recebe sobre o estado da sessão.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
    /// túnel SSH no ar ou indo direto: conectando ao Windows
    Connecting,
    Connected,
    /// 1ª conexão: o certificado foi aceito e deve ser guardado
    CertNew { fingerprint: String },
    /// certificado diferente do guardado: a senha não foi enviada
    CertChanged { fingerprint: String },
    /// fim da sessão; `error` vazio = desconectou normalmente
    Ended { error: String },
}

/// Entrada vinda da janela.
pub enum Input {
    Mouse { x: u16, y: u16 },
    Button { x: u16, y: u16, button: u8, down: bool },
    Wheel { x: u16, y: u16, delta: i16, horizontal: bool },
    /// scancode do teclado PC; bit 0x100 = tecla estendida (E0)
    Key { code: u16, down: bool },
    /// estado de Caps/Num/Scroll Lock ao focar a aba
    Sync { caps: bool, num: bool, scroll: bool },
    /// texto digitado como caracteres (Alt+P: a senha salva)
    Text(Zeroizing<String>),
    Resize { width: u16, height: u16 },
}

pub struct RdpView {
    input: mpsc::UnboundedSender<Input>,
    tunnel: std::sync::Arc<std::sync::Mutex<Option<Child>>>,
}

impl RdpView {
    /// Abre a sessão em segundo plano. `known_cert` é a impressão digital aceita antes
    /// (vazia na 1ª vez).
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        host: Host,
        gateway: Option<Host>,
        width: u16,
        height: u16,
        known_cert: String,
        frames: Channel<InvokeResponseBody>,
        events: Channel<Event>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let tunnel = std::sync::Arc::new(std::sync::Mutex::new(None));
        let tunnel_slot = tunnel.clone();
        thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = events.send(Event::Ended { error: e.to_string() });
                    return;
                }
            };
            let out = Out { frames, events: events.clone() };
            let result = rt.block_on(run(
                host, gateway, width, height, known_cert, rx, &out, &tunnel_slot,
            ));
            if let Some(mut c) = tunnel_slot.lock().unwrap().take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            let error = match result {
                Ok(()) => String::new(),
                Err(e) => format!("{e:#}"),
            };
            let _ = events.send(Event::Ended { error });
        });
        Self { input: tx, tunnel }
    }

    pub fn send(&self, input: Input) {
        let _ = self.input.send(input);
    }
}

impl Drop for RdpView {
    fn drop(&mut self) {
        // sem quem mande entrada, a sessão encerra; o túnel cai junto
        if let Some(mut c) = self.tunnel.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

struct Out {
    frames: Channel<InvokeResponseBody>,
    events: Channel<Event>,
}

impl Out {
    fn raw(&self, bytes: Vec<u8>) -> Result<()> {
        self.frames
            .send(InvokeResponseBody::Raw(bytes))
            .map_err(|_| anyhow!("janela fechada"))
    }
}

/// Impressão digital no formato do FreeRDP/ssh: `AB:CD:...`.
fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Kerberos não é usado: o CredSSP fica no NTLM, então não há KDC para chamar.
struct NoKdc;
impl ironrdp_tokio::NetworkClient for NoKdc {
    async fn send(
        &mut self,
        _: &ironrdp_connector::sspi::generator::NetworkRequest,
    ) -> ConnectorResult<Vec<u8>> {
        Err(ironrdp_connector::general_err!("Kerberos/KDC não suportado"))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    host: Host,
    gateway: Option<Host>,
    width: u16,
    height: u16,
    known_cert: String,
    mut rx: mpsc::UnboundedReceiver<Input>,
    out: &Out,
    tunnel: &std::sync::Mutex<Option<Child>>,
) -> Result<()> {
    if host.user.is_empty() {
        bail!("{}", sshdeck_core::lang::tr("err.rdp_user"));
    }
    let _ = out.events.send(Event::Connecting);

    // com túnel, o RDP vai para a ponta local dele
    let target: SocketAddr = match &gateway {
        Some(gw) => {
            let port = core_rdp::free_port()?;
            let child = core_rdp::tunnel_command(gw, port, &host.hostname, host.port)?
                .stderr(std::process::Stdio::piped())
                .spawn()
                .context(sshdeck_core::lang::tr("rdp.spawn_tunnel"))?;
            *tunnel.lock().unwrap() = Some(child);
            wait_tunnel(port, tunnel).await?;
            SocketAddr::from(([127, 0, 0, 1], port))
        }
        None => tokio::net::lookup_host((host.hostname.as_str(), host.port))
            .await?
            .next()
            .ok_or_else(|| anyhow!("{}: endereço não encontrado", host.hostname))?,
    };

    let stream = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(target))
        .await
        .map_err(|_| anyhow!("{}:{} não respondeu", host.hostname, host.port))??;
    stream.set_nodelay(true)?;
    let client_addr = stream.local_addr()?;

    let config = connector_config(&host, width, height);
    let drdynvc = ironrdp_dvc::DrdynvcClient::new()
        .with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));
    let mut connector =
        ironrdp_connector::ClientConnector::new(config, client_addr).with_static_channel(drdynvc);

    let mut framed = TokioFramed::new(stream);
    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector).await?;
    let (stream, leftover) = framed.into_inner();
    let (tls, cert) = ironrdp_tls::upgrade(stream, &host.hostname)
        .await
        .context("TLS")?;

    // a senha só sai depois daqui (CredSSP): o certificado precisa ser o mesmo de antes
    let der = x509_cert::der::Encode::to_der(&cert).context("certificado")?;
    let fp = fingerprint(&der);
    if known_cert.is_empty() {
        let _ = out.events.send(Event::CertNew {
            fingerprint: fp.clone(),
        });
    } else if known_cert != fp {
        let _ = out.events.send(Event::CertChanged { fingerprint: fp });
        return Ok(());
    }

    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = TokioFramed::new_with_leftover(tls, leftover);
    let public_key = ironrdp_tls::extract_tls_server_public_key(&cert)
        .ok_or_else(|| anyhow!("certificado sem chave pública"))?
        .to_owned();
    let result = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoKdc,
        ServerName::new(host.hostname.clone()),
        public_key,
        None,
    )
    .await
    .map_err(|e| anyhow!("{}", describe_connect_error(&e)))?;
    let _ = out.events.send(Event::Connected);
    session(framed, result, &mut rx, out).await
}

/// Espera a ponta local do túnel aceitar conexões (ou o ssh desistir).
async fn wait_tunnel(port: u16, tunnel: &std::sync::Mutex<Option<Child>>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return Ok(());
        }
        let exited = {
            let mut guard = tunnel.lock().unwrap();
            match guard.as_mut().map(|c| c.try_wait()) {
                Some(Ok(Some(_))) => {
                    let mut err = String::new();
                    if let Some(mut e) = guard.as_mut().and_then(|c| c.stderr.take()) {
                        use std::io::Read;
                        let _ = e.read_to_string(&mut err);
                    }
                    Some(err)
                }
                _ => None,
            }
        };
        if let Some(err) = exited {
            let last = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("ssh");
            bail!("{}", sshdeck_core::lang::tr("rdp.tunnel_down").replace("%{err}", last));
        }
        if Instant::now() > deadline {
            bail!("{}", sshdeck_core::lang::tr("rdp.tunnel_timeout"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn describe_connect_error(e: &ironrdp_connector::ConnectorError) -> String {
    // "[contexto] tipo: causa: causa…", sem o caminho do código-fonte do IronRDP
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text = format!("{text}: {s}");
        source = s.source();
    }
    // as falhas de login mais comuns, em vez do código do CredSSP
    let lower = text.to_lowercase();
    if lower.contains("logon_failure") || lower.contains("logondenied") || lower.contains("wrong_password") {
        return sshdeck_core::lang::tr("gui.rdp_logon_failed");
    }
    if lower.contains("account_restriction") || lower.contains("not_permitted") || lower.contains("account_disabled") {
        return sshdeck_core::lang::tr("gui.rdp_account_blocked");
    }
    text
}

fn connector_config(host: &Host, width: u16, height: u16) -> ironrdp_connector::Config {
    ironrdp_connector::Config {
        credentials: Credentials::UsernamePassword {
            username: host.user.clone(),
            password: host.password.clone(),
        },
        domain: (!host.domain.is_empty()).then(|| host.domain.clone()),
        // só NLA: a senha nunca vai para a tela de login gráfica do Windows
        enable_tls: false,
        enable_credssp: true,
        keyboard_type: ironrdp_pdu::gcc::KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        // 0 = o Windows usa o layout configurado na conta (os scancodes são da tecla física)
        keyboard_layout: 0,
        keyboard_functional_keys_count: 12,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        desktop_size: DesktopSize { width, height },
        desktop_scale_factor: 0,
        bitmap: Some(ironrdp_connector::BitmapConfig {
            color_depth: 32,
            lossy_compression: true,
            codecs: ironrdp_pdu::rdp::capability_sets::client_codecs_capabilities(&codec_flags())
                .unwrap_or_default(),
        }),
        client_build: 0,
        client_name: "sshdeck".into(),
        client_dir: String::new(),
        platform: MajorPlatformType::UNIX,
        hardware_id: None,
        license_cache: None,
        enable_server_pointer: true,
        autologon: false,
        enable_audio_playback: false,
        request_data: None,
        pointer_software_rendering: false,
        multitransport_flags: None,
        compression_type: None,
        // sem papel de parede nem animações: a tela chega mais rápido
        performance_flags: PerformanceFlags::DISABLE_WALLPAPER
            | PerformanceFlags::DISABLE_MENUANIMATIONS
            | PerformanceFlags::DISABLE_CURSOR_SHADOW
            | PerformanceFlags::ENABLE_FONT_SMOOTHING,
        timezone_info: Default::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
    }
}

/// Codecs anunciados: por padrão os do IronRDP (RemoteFX). `SSHDECK_RDP_CODECS=remotefx:off`
/// força bitmaps comuns, para servidores em que o RemoteFX não funciona bem.
fn codec_flags() -> Vec<&'static str> {
    match std::env::var("SSHDECK_RDP_CODECS") {
        Ok(v) => v
            .split(',')
            .map(|c| &*Box::leak(c.trim().to_string().into_boxed_str()))
            .filter(|c| !c.is_empty())
            .collect(),
        Err(_) => vec![],
    }
}

/// Retângulo sujo acumulado até o próximo envio.
#[derive(Clone, Copy)]
struct Dirty {
    left: u16,
    top: u16,
    right: u16,
    bottom: u16,
}

impl Dirty {
    fn add(this: &mut Option<Dirty>, r: &InclusiveRectangle) {
        *this = Some(match *this {
            None => Dirty {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            },
            Some(d) => Dirty {
                left: d.left.min(r.left),
                top: d.top.min(r.top),
                right: d.right.max(r.right),
                bottom: d.bottom.max(r.bottom),
            },
        });
    }

    fn all(image: &DecodedImage) -> Dirty {
        Dirty {
            left: 0,
            top: 0,
            right: image.width().saturating_sub(1),
            bottom: image.height().saturating_sub(1),
        }
    }
}

fn header(kind: u8, values: &[u16], extra: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + values.len() * 2 + extra);
    buf.push(kind);
    for v in values {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}

/// Copia o retângulo da tela decodificada e manda para a janela (alfa sempre opaco).
fn send_rect(out: &Out, image: &DecodedImage, d: Dirty) -> Result<()> {
    let iw = image.width();
    let ih = image.height();
    if iw == 0 || ih == 0 {
        return Ok(());
    }
    let right = d.right.min(iw - 1);
    let bottom = d.bottom.min(ih - 1);
    if d.left > right || d.top > bottom {
        return Ok(());
    }
    let (w, h) = (right - d.left + 1, bottom - d.top + 1);
    let stride = image.stride();
    let data = image.data();
    let mut buf = header(1, &[d.left, d.top, w, h], usize::from(w) * usize::from(h) * 4);
    for y in d.top..=bottom {
        let start = usize::from(y) * stride + usize::from(d.left) * 4;
        let row = &data[start..start + usize::from(w) * 4];
        for px in row.as_chunks::<4>().0 {
            buf.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    out.raw(buf)
}

fn input_events(input: Input) -> Vec<FastPathInputEvent> {
    let mouse = |flags: PointerFlags, x: u16, y: u16, units: i16| {
        FastPathInputEvent::MouseEvent(MousePdu {
            flags,
            number_of_wheel_rotation_units: units,
            x_position: x,
            y_position: y,
        })
    };
    match input {
        Input::Mouse { x, y } => vec![mouse(PointerFlags::MOVE, x, y, 0)],
        Input::Button { x, y, button, down } => {
            let flag = match button {
                0 => PointerFlags::LEFT_BUTTON,
                1 => PointerFlags::MIDDLE_BUTTON_OR_WHEEL,
                2 => PointerFlags::RIGHT_BUTTON,
                _ => return vec![],
            };
            let flags = if down { flag | PointerFlags::DOWN } else { flag };
            vec![mouse(flags, x, y, 0)]
        }
        Input::Wheel {
            x,
            y,
            delta,
            horizontal,
        } => {
            let flag = if horizontal {
                PointerFlags::HORIZONTAL_WHEEL
            } else {
                PointerFlags::VERTICAL_WHEEL
            };
            vec![mouse(flag, x, y, delta.clamp(-255, 255))]
        }
        Input::Key { code, down } => {
            let mut flags = KeyboardFlags::empty();
            if code & 0x100 != 0 {
                flags |= KeyboardFlags::EXTENDED;
            }
            if !down {
                flags |= KeyboardFlags::RELEASE;
            }
            vec![FastPathInputEvent::KeyboardEvent(flags, (code & 0xff) as u8)]
        }
        Input::Sync { caps, num, scroll } => {
            let mut flags = SynchronizeFlags::empty();
            flags.set(SynchronizeFlags::CAPS_LOCK, caps);
            flags.set(SynchronizeFlags::NUM_LOCK, num);
            flags.set(SynchronizeFlags::SCROLL_LOCK, scroll);
            vec![FastPathInputEvent::SyncEvent(flags)]
        }
        Input::Text(text) => text
            .encode_utf16()
            .flat_map(|c| {
                [
                    FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::empty(), c),
                    FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::RELEASE, c),
                ]
            })
            .collect(),
        Input::Resize { .. } => vec![],
    }
}

async fn session<S>(
    framed: TokioFramed<S>,
    result: ConnectionResult,
    rx: &mut mpsc::UnboundedReceiver<Input>,
    out: &Out,
) -> Result<()>
where
    S: Sync + Send + Unpin + tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    let (mut reader, mut writer) = split_tokio_framed(framed);
    let size = result.desktop_size;
    let mut image = DecodedImage::new(PixelFormat::RgbA32, size.width, size.height);
    let activation_factory = result.activation_factory;
    let mut stage: ActiveStage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();

    out.raw(header(2, &[size.width, size.height], 0))?;
    let mut dirty = Some(Dirty::all(&image));
    // junta as atualizações em quadros de ~16 ms: menos mensagens, mesmo resultado
    let mut flush = tokio::time::interval(Duration::from_millis(16));
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let outputs = tokio::select! {
            frame = reader.read_pdu() => {
                let (action, payload) = frame.context("conexão RDP")?;
                stage.process(&mut image, action, &payload)?
            }
            input = rx.recv() => {
                // sem a aba (fechada), encerra educadamente
                let Some(input) = input else {
                    let frames = stage.graceful_shutdown()?;
                    for o in frames {
                        if let ActiveStageOutput::ResponseFrame(f) = o {
                            let _ = writer.write_all(&f).await;
                        }
                    }
                    return Ok(());
                };
                if let Input::Resize { width, height } = input {
                    let (w, h) = MonitorLayoutEntry::adjust_display_size(width.into(), height.into());
                    match stage.encode_resize(w, h, None, None) {
                        Some(frame) => vec![ActiveStageOutput::ResponseFrame(frame?)],
                        None => vec![],
                    }
                } else {
                    let events = input_events(input);
                    if events.is_empty() { vec![] } else { stage.process_fastpath_input(&mut image, &events)? }
                }
            }
            _ = flush.tick() => {
                if let Some(d) = dirty.take() {
                    send_rect(out, &image, d)?;
                }
                vec![]
            }
        };

        for o in outputs {
            match o {
                ActiveStageOutput::ResponseFrame(f) => {
                    writer.write_all(&f).await.context("envio ao servidor")?
                }
                ActiveStageOutput::GraphicsUpdate(r) => Dirty::add(&mut dirty, &r),
                ActiveStageOutput::PointerDefault => out.raw(vec![4])?,
                ActiveStageOutput::PointerHidden => out.raw(vec![5])?,
                ActiveStageOutput::PointerPosition { .. } => {}
                ActiveStageOutput::PointerBitmap(p) => {
                    let mut buf = header(
                        3,
                        &[p.width, p.height, p.hotspot_x, p.hotspot_y],
                        p.bitmap_data.len(),
                    );
                    buf.extend_from_slice(&p.bitmap_data);
                    out.raw(buf)?;
                }
                ActiveStageOutput::DeactivateAll => {
                    // o servidor mudou a resolução (ou reconectou a sessão): refaz a ativação
                    let mut activation = activation_factory.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let written =
                            single_sequence_step_read(&mut reader, &mut activation, &mut buf).await?;
                        if written.size().is_some() {
                            writer.write_all(buf.filled()).await?;
                        }
                        if let ConnectionActivationState::Finalized {
                            desktop_size,
                            share_id,
                            enable_server_pointer,
                            pointer_software_rendering,
                        } = activation.connection_activation_state()
                        {
                            image = DecodedImage::new(
                                PixelFormat::RgbA32,
                                desktop_size.width,
                                desktop_size.height,
                            );
                            stage.set_fastpath_processor(
                                fast_path::ProcessorBuilder {
                                    io_channel_id: activation.io_channel_id(),
                                    user_channel_id: activation.user_channel_id(),
                                    share_id,
                                    enable_server_pointer,
                                    pointer_software_rendering,
                                    bulk_decompressor: None,
                                }
                                .build(),
                            );
                            stage.set_share_id(share_id);
                            stage.set_enable_server_pointer(enable_server_pointer);
                            out.raw(header(2, &[desktop_size.width, desktop_size.height], 0))?;
                            dirty = Some(Dirty::all(&image));
                            break;
                        }
                    }
                }
                ActiveStageOutput::Terminate(_) => return Ok(()),
                _ => {}
            }
        }
    }
}
