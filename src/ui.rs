use crate::{
    app::{App, Field, Form, Mode, PwPurpose},
    lang,
    vault::{Host, Kind},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, Wrap},
};
use rust_i18n::t;
use std::borrow::Cow;
use tui_term::widget::{Cursor, PseudoTerminal};

const ACCENT: Color = Color::Cyan;
/// nf-fa-lock (Nerd Fonts) — marca hosts com senha salva
const KEY_ICON: &str = "\u{f023}";
/// nf-fa-terminal / nf-fa-windows — tipo do host
const SSH_ICON: &str = "\u{f120}";
const RDP_ICON: &str = "\u{f17a}";

/// Ícone do tipo: verde quando há conexão aberta (negrito se for a ativa). Na linha
/// selecionada, sem conexão, usa a cor do texto: cinza sobre o fundo cinza sumia.
fn kind_icon(h: &Host, connected: bool, active: bool, selected: bool) -> Span<'static> {
    let icon = match h.kind {
        Kind::Ssh => SSH_ICON,
        Kind::Rdp => RDP_ICON,
    };
    let style = match (connected, active) {
        (true, true) => Style::new().fg(Color::Green).bold(),
        (true, false) => Style::new().fg(Color::Green),
        _ if selected => Style::new(),
        _ => Style::new().dark_gray(),
    };
    Span::styled(icon, style)
}

/// Coluna "Extra" da lista: chave do SSH ou domínio/túnel do RDP.
fn extra(h: &Host) -> String {
    match h.kind {
        Kind::Ssh => h.identity_file.clone(),
        Kind::Rdp => {
            let mut parts = Vec::new();
            if !h.domain.is_empty() {
                parts.push(t!("ui.extra_domain", d = h.domain));
            }
            if !h.gateway.is_empty() {
                parts.push(t!("ui.extra_via", gw = h.gateway));
            }
            parts.join(" · ")
        }
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    draw_footer(f, app, footer);

    if let Mode::Password {
        purpose,
        input,
        first,
        error,
    } = &app.mode
    {
        draw_password(
            f,
            *purpose,
            input.chars().count(),
            first.is_some(),
            error.as_deref(),
        );
        return;
    }

    let [sidebar, term] = workspace_columns(main);
    // tamanho do terminal embutido = área interna do painel central
    app.term_size = (term.height.saturating_sub(2), term.width.saturating_sub(2));
    app.term_area = term;

    if app.sessions.is_empty() {
        draw_list(f, app, main);
    } else {
        draw_sidebar(f, app, sidebar);
        draw_terminal(f, app, term);
    }
    draw_status(f, app, main);

    match &app.mode {
        Mode::Form(form) => draw_form(f, form),
        Mode::ConfirmDelete(i) => draw_confirm(
            f,
            &t!("ui.delete_title"),
            &t!("ui.delete_q", alias = app.store.hosts[*i].alias),
        ),
        Mode::ConfirmQuit => draw_confirm(
            f,
            &t!("ui.quit_title"),
            &t!("ui.quit_q", n = app.open_count()),
        ),
        Mode::ConfirmSysinfo(alias) => draw_question(
            f,
            &t!("ui.sysinfo_title"),
            &t!("ui.sysinfo_q", alias = alias),
            ACCENT,
        ),
        Mode::Help => draw_help(f),
        Mode::Lang(i) => draw_lang(f, *i),
        _ => {}
    }
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.visible();
    let selected = app.table.selected();
    let rows = visible.iter().enumerate().map(|(pos, &i)| {
        let h = &app.store.hosts[i];
        let sel = selected == Some(pos);
        let key = if h.password.is_empty() {
            Span::raw("  ")
        } else {
            Span::raw(KEY_ICON)
        };
        Row::new(vec![
            Cell::from(kind_icon(h, app.has_session(&h.alias), false, sel)),
            Cell::from(key),
            Cell::from(h.alias.clone()).bold(),
            Cell::from(h.user.clone()),
            Cell::from(h.hostname.clone()),
            Cell::from(h.port.to_string()),
            if sel {
                Cell::from(extra(h))
            } else {
                Cell::from(extra(h)).dim()
            },
        ])
    });

    let mut title = vec![Span::styled(" sshdeck ", Style::new().fg(ACCENT).bold())];
    title.push(Span::raw(format!("· {} hosts ", app.store.hosts.len())));
    if !app.filter.is_empty() || matches!(app.mode, Mode::Filter) {
        title.push(Span::styled(
            format!("· {}", t!("ui.filter", f = app.filter)),
            Style::new().fg(Color::Yellow),
        ));
        if matches!(app.mode, Mode::Filter) {
            title.push(Span::styled("▏", Style::new().fg(Color::Yellow)));
        }
        title.push(Span::raw(" "));
    }

    let [alias, user, host, port, extra] = list_widths(area.width);
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(alias),
            Constraint::Length(user),
            Constraint::Length(host),
            Constraint::Length(port),
            Constraint::Length(extra),
        ],
    )
    .header(
        Row::new(vec![
            Cow::Borrowed(""),
            Cow::Borrowed(""),
            t!("field.alias"),
            t!("ui.col_user"),
            t!("field.hostname"),
            t!("ui.col_port"),
            t!("ui.col_extra"),
        ])
        .style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
        .bottom_margin(1),
    )
    .row_highlight_style(
        Style::new()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Line::from(title));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if visible.is_empty() {
        let msg = if app.store.hosts.is_empty() {
            t!("ui.empty")
        } else {
            t!("ui.no_match")
        };
        f.render_widget(table, inner);
        f.render_widget(Paragraph::new(msg).centered().dim(), centered(area, 60, 5));
        return;
    }

    // a tabela ocupa só a largura das colunas ("▶ " e 1 espaço entre as 7 colunas);
    // à direita, sem caixa, vão as notas, a chave e a última conexão do host selecionado
    let table_width = alias + user + host + port + extra + 1 + 1 + 2 + 6;
    let [table_area, _, side] = Layout::horizontal([
        Constraint::Length(table_width),
        Constraint::Length(4),
        Constraint::Fill(1),
    ])
    .areas(inner);
    if side.width >= SIDE_MIN {
        f.render_stateful_widget(table, table_area, &mut app.table);
        let side = Rect {
            width: side.width.saturating_sub(1),
            ..side
        };
        if let Some(i) = app.selected_host() {
            let h = &app.store.hosts[i];
            draw_host_side(
                f,
                h,
                app.connection(&h.alias),
                app.sysinfo_busy(&h.alias),
                side,
            );
        }
    } else {
        f.render_stateful_widget(table, inner, &mut app.table);
    }
}

/// Largura mínima para mostrar as informações ao lado da lista.
const SIDE_MIN: u16 = 28;

/// Notas, chave e última conexão do host selecionado, numa caixa ao lado da lista
/// (com a altura do conteúdo).
fn draw_host_side(f: &mut Frame, h: &Host, connection: Option<bool>, collecting: bool, area: Rect) {
    let when = |t: Option<u64>, missing: Cow<'static, str>| match t {
        Some(t) => Span::raw(ago(t)),
        None => Span::raw(missing).dark_gray(),
    };
    let mut rows: Vec<(Cow<str>, Span)> = Vec::new();
    // dados coletados do servidor (tecla U, ou o popup da 1ª conexão); sempre visíveis nos
    // hosts SSH, em cinza enquanto não houver, para lembrar que dá para coletar
    if h.kind == Kind::Ssh {
        let gray = |text: Cow<'static, str>| Span::raw(text).dark_gray();
        let missing = || {
            if collecting {
                Span::styled(t!("info.collecting"), Style::new().fg(Color::Yellow))
            } else {
                gray("Shift+U".into())
            }
        };
        let i = h.sysinfo.as_ref();
        let value = |v: Option<String>| match v.filter(|v| !v.is_empty()) {
            Some(v) => Span::raw(v),
            None if i.is_some() => gray("-".into()),
            None => missing(),
        };
        rows.push((t!("info.system"), value(i.map(|i| i.os.clone()))));
        rows.push((t!("info.hostname"), value(i.map(|i| i.hostname.clone()))));
        rows.push((
            t!("info.kernel"),
            value(i.map(|i| format!("{} ({})", i.kernel, i.arch))),
        ));
        rows.push((
            t!("info.hardware"),
            value(i.map(|i| {
                let mut hw = Vec::new();
                if let Some(n) = i.cpus {
                    hw.push(format!("{n} CPU"));
                }
                if let Some(kb) = i.mem_kb {
                    hw.push(format!("{:.1} GB", kb as f64 / 1024.0 / 1024.0));
                }
                hw.join(" · ")
            })),
        ));
        rows.push((
            t!("info.collected"),
            match i {
                Some(i) if collecting => Span::styled(
                    format!(
                        "{} ({})",
                        local_datetime(i.collected),
                        t!("info.collecting")
                    ),
                    Style::new().fg(Color::Yellow),
                ),
                Some(i) => Span::raw(local_datetime(i.collected)),
                None => missing(),
            },
        ));
    }
    // SSH e RDP com o mesmo número de linhas, para a caixa não mudar de tamanho
    rows.push(match h.kind {
        Kind::Ssh => (
            t!("info.key"),
            if h.identity_file.is_empty() {
                Span::raw(t!("info.key_default")).dark_gray()
            } else {
                Span::raw(h.identity_file.clone())
            },
        ),
        Kind::Rdp => (
            t!("info.resolution"),
            if h.resolution.is_empty() {
                Span::raw(t!("info.res_default")).dark_gray()
            } else {
                Span::raw(h.resolution.clone())
            },
        ),
    });
    rows.push((t!("info.last"), when(h.last_used, t!("info.never"))));
    let date = |t: Option<u64>, missing: Cow<'static, str>| match t {
        Some(t) => Span::raw(local_datetime(t)),
        None => Span::raw(missing).dark_gray(),
    };
    rows.push((t!("info.created"), date(h.created, "-".into())));
    rows.push((t!("info.updated"), date(h.updated, t!("info.never"))));
    let width = rows
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0)
        + 2;

    let mut lines: Vec<Line> = Vec::new();
    lines.extend(rows.into_iter().map(|(label, value)| {
        Line::from(vec![
            Span::styled(format!("{label:<width$}"), Style::new().dark_gray()),
            value,
        ])
    }));
    if !h.notes.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            t!("field.notes").to_string(),
            Style::new().fg(ACCENT).bold(),
        ));
        lines.extend(h.notes.lines().map(|l| Line::from(l.to_string())));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .padding(ratatui::widgets::Padding::horizontal(1))
        .title(Line::from(
            [
                vec![Span::styled(
                    format!(" {} ", h.alias),
                    Style::new().fg(ACCENT).bold(),
                )],
                match connection {
                    Some(true) => vec![Span::styled(
                        format!("{DOT} {} ", t!("info.connected")),
                        Style::new().fg(Color::Green).bold(),
                    )],
                    Some(false) => vec![Span::styled(
                        format!("{DOT} {} ", t!("info.connecting")),
                        Style::new().fg(Color::Yellow),
                    )],
                    None => vec![],
                },
            ]
            .concat(),
        ));
    // altura = linhas já quebradas na largura da caixa + bordas
    let text_width = area.width.saturating_sub(4).max(1) as usize;
    let height: usize = lines
        .iter()
        .map(|l| l.width().div_ceil(text_width).max(1))
        .sum();
    let area = Rect {
        height: (height as u16 + 2).min(area.height),
        ..area
    };
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

/// nf-oct-dot_fill: separa a data da hora
const DOT: &str = "\u{f444}";

/// "13/10/2026 ● 14:59:11", no fuso do sistema.
fn local_datetime(secs: u64) -> String {
    match chrono::DateTime::from_timestamp(secs as i64, 0) {
        Some(utc) => utc
            .with_timezone(&chrono::Local)
            .format(&format!("%d/%m/%Y {DOT} %H:%M:%S"))
            .to_string(),
        None => "-".into(),
    }
}

/// "há 5 min", "há 2 h", "há 3 dia(s)"
fn ago(then: u64) -> String {
    let secs = crate::vault::now().saturating_sub(then);
    match secs {
        0..60 => t!("info.ago_now").into(),
        60..3600 => t!("info.ago_min", n = secs / 60).into(),
        3600..86400 => t!("info.ago_h", n = secs / 3600).into(),
        _ => t!("info.ago_d", n = secs / 86400).into(),
    }
}

const SIDEBAR_WIDTH: u16 = 34;

fn workspace_columns(area: Rect) -> [Rect; 2] {
    Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(20)]).areas(area)
}

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::new().fg(ACCENT)
    } else {
        Style::new().dark_gray()
    }
}

/// Larguras de Alias, User, HostName, Port e Extra: crescem junto com o terminal, cada uma
/// entre um mínimo e um máximo, para as colunas não ficarem longe demais em telas largas.
fn list_widths(total: u16) -> [u16; 5] {
    // (mínimo, máximo, peso)
    const COLS: [(u16, u16, u16); 5] = [
        (12, 24, 3),
        (10, 16, 2),
        (15, 20, 2),
        (6, 10, 1),
        (10, 30, 3),
    ];
    // bordas, "▶ ", os dois ícones e 1 espaço entre as 7 colunas
    let free = total.saturating_sub(2 + 2 + 2 + 6);
    let weights: u32 = COLS.iter().map(|c| c.2 as u32).sum();
    let mut widths =
        COLS.map(|(min, max, w)| ((free as u32 * w as u32 / weights) as u16).clamp(min, max));
    // os mínimos podem passar da tela: quem cede é a Extra, a menos importante
    let over = widths.iter().sum::<u16>().saturating_sub(free);
    widths[4] = widths[4].saturating_sub(over);
    widths
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let [list_area, info_area] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(14)]).areas(area);

    let active_alias = app.active_session().map(|s| s.alias.clone());
    let visible = app.visible();
    let selected = app.table.selected();
    let rows: Vec<Row> = visible
        .iter()
        .enumerate()
        .map(|(pos, &i)| {
            let h = &app.store.hosts[i];
            let sel = selected == Some(pos);
            let active = Some(&h.alias) == active_alias.as_ref();
            let key = if h.password.is_empty() { "" } else { KEY_ICON };
            Row::new(vec![
                Cell::from(kind_icon(h, app.has_session(&h.alias), active, sel)),
                Cell::from(key),
                Cell::from(h.alias.clone()).bold(),
                if sel {
                    Cell::from(h.hostname.clone())
                } else {
                    Cell::from(h.hostname.clone()).dim()
                },
            ])
        })
        .collect();

    let mut title = vec![Span::styled(
        format!(" {} ", t!("ui.servers")),
        Style::new().fg(ACCENT).bold(),
    )];
    if !app.filter.is_empty() || matches!(app.mode, Mode::Filter) {
        title.push(Span::styled(
            format!("/{}", app.filter),
            Style::new().fg(Color::Yellow),
        ));
        title.push(Span::raw(" "));
    }
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Min(6),
        ],
    )
    .row_highlight_style(Style::new().bg(Color::DarkGray))
    .highlight_symbol("▶")
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(focus_border(!app.term_focus))
            .title(Line::from(title)),
    );
    f.render_stateful_widget(table, list_area, &mut app.table);

    draw_info(f, app, info_area);
}

fn draw_info(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().dark_gray())
        .title(Span::styled(
            format!(" {} ", t!("ui.connection")),
            Style::new().fg(ACCENT).bold(),
        ));
    let Some(s) = app.active_session() else {
        f.render_widget(block, area);
        return;
    };
    let labels = [
        t!("info.user"),
        t!("info.host"),
        t!("info.port"),
        t!("info.key"),
        t!("info.password"),
        t!("info.time"),
    ];
    // largura da coluna de rótulos depende do idioma ("contraseña" é maior que "senha")
    let width = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0) + 1;
    let field = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<width$}"), Style::new().dark_gray()),
            Span::raw(v),
        ])
    };
    let [l_user, l_host, l_port, l_key, l_pw, l_time] = &labels;
    let mut lines = vec![Line::from(Span::styled(
        s.alias.clone(),
        Style::new().fg(Color::Green).bold(),
    ))];
    if let Some(h) = app.host_by_alias(&s.alias) {
        lines.push(field(
            l_user,
            if h.user.is_empty() {
                "-".into()
            } else {
                h.user.clone()
            },
        ));
        lines.push(field(l_host, h.hostname.clone()));
        lines.push(field(l_port, h.port.to_string()));
        if !h.identity_file.is_empty() {
            lines.push(field(l_key, h.identity_file.clone()));
        }
        lines.push(field(
            l_pw,
            if h.password.is_empty() {
                t!("info.not_saved").into()
            } else {
                t!("info.saved", icon = KEY_ICON).into()
            },
        ));
        lines.push(field(l_time, elapsed(s.started.elapsed().as_secs())));
        if !h.notes.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(h.notes.clone()).italic());
        }
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

fn elapsed(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn draw_terminal(f: &mut Frame, app: &mut App, area: Rect) {
    let (rows, cols) = app.term_size;
    let focused = app.term_focus;
    let n = app.sessions.len();
    let idx = app.active;
    let Some(session) = app.sessions.get_mut(idx) else {
        return;
    };
    session.resize(rows, cols);

    let parser = session.parser.lock().unwrap();
    let screen = parser.screen();
    let mut title = vec![
        Span::styled(
            format!(" ssh {} ", session.alias),
            Style::new().fg(ACCENT).bold(),
        ),
        Span::raw(format!("[{}/{}] ", idx + 1, n)).dark_gray(),
    ];
    if screen.scrollback() > 0 {
        title.push(Span::styled(
            format!("{} ", t!("ui.history", n = screen.scrollback())),
            Style::new().fg(Color::Yellow),
        ));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focus_border(focused))
        .title(Line::from(title));
    let mut cursor = Cursor::default();
    if !focused || screen.scrollback() > 0 {
        cursor.hide();
    }
    f.render_widget(
        PseudoTerminal::new(screen).block(block).cursor(cursor),
        area,
    );
}

type Hint = (Cow<'static, str>, Cow<'static, str>);

fn k(key: impl Into<Cow<'static, str>>, desc: impl Into<Cow<'static, str>>) -> Hint {
    (key.into(), desc.into())
}

/// Rodapé com os atalhos do contexto atual — sempre visível.
fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let keys: Vec<Hint> = match &app.mode {
        Mode::Password { purpose, .. } => vec![
            k("Enter", t!("h.confirm")),
            k(
                "Esc",
                if *purpose == PwPurpose::Rekey {
                    t!("h.cancel")
                } else {
                    t!("h.quit")
                },
            ),
        ],
        Mode::Form(form) if form.focused() == Field::Kind => vec![
            k(t!("key.arrows_space"), "SSH/RDP"),
            k("Tab/↓", t!("h.next")),
            k("Ctrl+S", t!("h.save")),
            k("Esc", t!("h.cancel")),
        ],
        Mode::Form(_) => vec![
            k("Tab/↓", t!("h.next")),
            k("Shift+Tab/↑", t!("h.prev")),
            k("Ctrl+S", t!("h.save")),
            k("Ctrl+R", t!("h.show_pw")),
            k("Ctrl+U", t!("h.clear_field")),
            k("Esc", t!("h.cancel")),
        ],
        Mode::ConfirmDelete(_) | Mode::ConfirmQuit | Mode::ConfirmSysinfo(_) => confirm_hints(),
        Mode::Help => vec![k(t!("key.any"), t!("h.close"))],
        Mode::Lang(_) => vec![
            k("↑/↓", t!("h.choose")),
            k("Enter", t!("h.ok")),
            k("Esc", t!("h.cancel")),
        ],
        Mode::Filter => vec![
            k(t!("key.type"), t!("h.filter")),
            k("Enter", t!("h.ok")),
            k("Esc", t!("h.clear")),
        ],
        Mode::List if app.term_focus && !app.sessions.is_empty() => vec![
            k("Ctrl+]", t!("h.panel")),
            k("Alt+P", t!("h.type_pw")),
            k("Alt+←/→", t!("h.switch_session")),
            k("Shift+PgUp/PgDn", t!("h.history")),
        ],
        Mode::List if !app.sessions.is_empty() => vec![
            k("Enter", t!("h.connect")),
            k("Tab", t!("h.terminal")),
            k("x", t!("h.close_session")),
            k("K", "ssh-copy-id"),
            k("c", t!("h.copy_pw")),
            k("a/e/d", t!("h.add_edit_del")),
            k("/", t!("h.filter")),
            k("?", t!("h.help")),
            k("q", t!("h.quit")),
        ],
        Mode::List => vec![
            k("Enter", t!("h.connect")),
            k("x", t!("h.close")),
            k("s", t!("h.fullscreen")),
            k("K", "ssh-copy-id"),
            k("c", t!("h.copy_pw")),
            k("a/e/d", t!("h.add_edit_del")),
            k("i", t!("h.import")),
            k("U", t!("h.sysinfo")),
            k("/", t!("h.filter")),
            k("?", t!("h.help")),
            k("q", t!("h.quit")),
        ],
    };
    f.render_widget(Paragraph::new(hints(&keys)), area);
}

fn confirm_hints() -> Vec<Hint> {
    vec![
        k(t!("key.yes"), t!("h.yes")),
        k(t!("key.other"), t!("h.no")),
    ]
}

/// Mensagem de status no canto inferior direito da borda, sem esconder os atalhos.
fn draw_status(f: &mut Frame, app: &App, main: Rect) {
    let Some((msg, _)) = &app.status else {
        return;
    };
    if main.height < 2 || main.width < 6 {
        return;
    }
    let area = Rect {
        x: main.x + 2,
        y: main.bottom() - 1,
        width: main.width - 4,
        height: 1,
    };
    f.render_widget(
        Line::from(Span::styled(
            format!(" {msg} "),
            Style::new().fg(Color::Green).bold(),
        ))
        .right_aligned(),
        area,
    );
}

fn hints(keys: &[Hint]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, (k, d)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" │ ").dark_gray());
        }
        spans.push(Span::styled(k.to_string(), Style::new().fg(ACCENT).bold()));
        spans.push(Span::raw(format!(" {d}")));
    }
    Line::from(spans)
}

fn draw_password(
    f: &mut Frame,
    purpose: PwPurpose,
    len: usize,
    confirming: bool,
    error: Option<&str>,
) {
    let area = centered(f.area(), 54, 9);
    f.render_widget(Clear, area);
    let title = match purpose {
        PwPurpose::Unlock => t!("pw.title_unlock"),
        PwPurpose::Create => t!("pw.title_create"),
        PwPurpose::Rekey => t!("pw.title_rekey"),
    };
    let prompt = match (purpose, confirming) {
        (PwPurpose::Unlock, _) => t!("pw.prompt_unlock"),
        (_, false) => t!("pw.prompt_define"),
        (_, true) => t!("pw.prompt_confirm"),
    };
    let mut lines = vec![
        Line::from(prompt),
        Line::from(""),
        Line::from(vec![
            Span::styled("•".repeat(len), Style::new().fg(ACCENT)),
            Span::styled("▏", Style::new().fg(ACCENT)),
        ]),
        Line::from(""),
    ];
    match error {
        Some(e) => lines.push(Line::from(Span::styled(
            e.to_string(),
            Style::new().fg(Color::Red),
        ))),
        None if purpose == PwPurpose::Create => lines.push(Line::from(t!("pw.warning")).dim()),
        None => lines.push(Line::from("")),
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(ACCENT).bold(),
        ));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_form(f: &mut Frame, form: &Form) {
    let fields = form.fields();
    let area = centered(f.area(), 84, fields.len() as u16 + 5);
    f.render_widget(Clear, area);
    let title = if form.editing.is_some() {
        t!("form.edit")
    } else {
        t!("form.new")
    };
    let port_hint = form.default_port().to_string();
    let mut lines = vec![Line::from("")];
    for (i, &field) in fields.iter().enumerate() {
        let focused = i == form.focus;
        let label_style = if focused {
            Style::new().fg(ACCENT).bold()
        } else {
            Style::new().dim()
        };
        let mut spans = vec![
            Span::styled(if focused { "▶ " } else { "  " }, label_style),
            Span::styled(format!("{:<16}", field.label()), label_style),
        ];

        if field == Field::Kind {
            for (k, name) in [(Kind::Ssh, " SSH "), (Kind::Rdp, " RDP ")] {
                spans.push(if form.kind == k {
                    Span::styled(name, Style::new().fg(Color::Black).bg(ACCENT).bold())
                } else {
                    Span::raw(name).dark_gray()
                });
                spans.push(Span::raw(" "));
            }
            if focused {
                spans.push(Span::raw(format!(" {}", t!("form.toggle"))).dark_gray());
            }
            lines.push(Line::from(spans));
            continue;
        }

        let raw = form.value(field);
        let value = if field == Field::Password && !form.show_pw {
            "•".repeat(raw.chars().count())
        } else {
            raw.to_string()
        };
        let placeholder: Option<Cow<str>> = match field {
            Field::Port => Some(port_hint.as_str().into()),
            Field::IdentityFile => Some(form.identity_hint.as_str().into()),
            Field::Domain => Some(t!("ph.domain")),
            Field::Resolution => Some(t!("ph.resolution")),
            Field::Share => Some(t!("ph.share")),
            Field::Gateway => Some(t!("ph.gateway")),
            _ => None,
        };
        // com placeholder, o cursor fica antes do texto cinza (o campo está vazio)
        if focused && raw.is_empty() {
            spans.push(Span::styled("▏", Style::new().fg(ACCENT)));
        }
        match placeholder {
            Some(p) if raw.is_empty() => spans.push(Span::raw(p.to_string()).dark_gray()),
            _ => spans.push(Span::raw(value)),
        }
        if focused && !raw.is_empty() {
            spans.push(Span::styled("▏", Style::new().fg(ACCENT)));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    match &form.error {
        Some(e) => lines.push(Line::from(Span::styled(
            format!("  {e}"),
            Style::new().fg(Color::Red),
        ))),
        None => lines.push(Line::from("")),
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(ACCENT).bold(),
        ));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_confirm(f: &mut Frame, title: &str, question: &str) {
    draw_question(f, title, question, Color::Red);
}

/// Popup com uma pergunta (pode ter várias linhas) e os atalhos de sim/não.
fn draw_question(f: &mut Frame, title: &str, question: &str, color: Color) {
    let widest = question
        .lines()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0);
    let lines = question.lines().count() as u16;
    let area = centered(f.area(), (widest as u16 + 6).max(40), lines + 4);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .padding(ratatui::widgets::Padding::horizontal(1))
        .title(format!(" {title} "));
    let mut text: Vec<Line> = question
        .lines()
        .map(|l| Line::from(l.to_string()))
        .collect();
    text.push(Line::from(""));
    text.push(hints(&confirm_hints()));
    f.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_lang(f: &mut Frame, selected: usize) {
    let current = lang::current();
    let lines: Vec<Line> = lang::LANGS
        .iter()
        .zip(lang::NAMES)
        .enumerate()
        .map(|(i, (code, name))| {
            let mark = if *code == current { "●" } else { " " };
            let text = format!(" {mark} {name} ");
            if i == selected {
                Line::styled(text, Style::new().fg(Color::Black).bg(ACCENT).bold())
            } else {
                Line::raw(text)
            }
        })
        .collect();
    let area = centered(f.area(), 30, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(format!(" {} ", t!("ui.lang_title")));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_help(f: &mut Frame) {
    let rows: Vec<Hint> = vec![
        k("Enter", t!("help.enter")),
        k("s", t!("help.s")),
        k("Tab / l", t!("help.tab")),
        k("x", t!("help.x")),
        k("K", t!("help.k")),
        k("c / y", t!("help.c")),
        k("a", t!("help.a")),
        k("e", t!("help.e")),
        k("d / Del", t!("help.d")),
        k("/", t!("help.filter")),
        k("i", t!("help.i")),
        k("U", t!("help.u")),
        k("P", t!("help.rekey")),
        k("L", t!("help.lang")),
        k("j/k ↑/↓ g/G", t!("help.nav")),
        k("q / Esc", t!("help.quit")),
        k("", ""),
        k(t!("help.in_terminal"), ""),
        k("Ctrl+]", t!("help.panel")),
        k("Alt+P", t!("help.altp")),
        k("Alt+← / →", t!("help.switch")),
        k("Shift+PgUp/Dn", t!("help.scroll")),
        k("", ""),
        k(t!("help.tip_key"), t!("help.tip")),
    ];
    let area = centered(f.area(), 90, rows.len() as u16 + 4);
    f.render_widget(Clear, area);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("  {k:<16}"), Style::new().fg(ACCENT).bold()),
                Span::raw(d.clone()),
            ])
        })
        .collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" {} ", t!("help.title")),
            Style::new().fg(ACCENT).bold(),
        ))
        .title_bottom(Line::from(format!(" {} ", t!("help.close"))).dim());
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::list_widths;

    #[test]
    fn columns_grow_with_terminal_up_to_max() {
        for total in [60, 80, 100, 120] {
            let used: u16 = list_widths(total).iter().sum::<u16>() + 12;
            assert!(used <= total.max(64), "{total}: {used}");
        }

        let mid = list_widths(120);
        assert!(mid.iter().zip(list_widths(80)).all(|(a, b)| *a >= b));
        assert_eq!(list_widths(250), [24, 16, 20, 10, 30]);
        assert_eq!(list_widths(u16::MAX), [24, 16, 20, 10, 30]);
    }
}
