use crate::{
    app::{App, Field, Form, Mode, PwPurpose},
    vault::{Host, Kind},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, Wrap},
};
use tui_term::widget::{Cursor, PseudoTerminal};

const ACCENT: Color = Color::Cyan;
/// nf-fa-lock (Nerd Fonts) — marca hosts com senha salva
const KEY_ICON: &str = "\u{f023}";
/// nf-fa-terminal / nf-fa-windows — tipo do host
const SSH_ICON: &str = "\u{f120}";
const RDP_ICON: &str = "\u{f17a}";

/// Ícone do tipo: verde quando há conexão aberta (negrito se for a ativa).
fn kind_icon(h: &Host, connected: bool, active: bool) -> Span<'static> {
    let icon = match h.kind {
        Kind::Ssh => SSH_ICON,
        Kind::Rdp => RDP_ICON,
    };
    let style = match (connected, active) {
        (true, true) => Style::new().fg(Color::Green).bold(),
        (true, false) => Style::new().fg(Color::Green),
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
                parts.push(format!("domínio {}", h.domain));
            }
            if !h.gateway.is_empty() {
                parts.push(format!("via {}", h.gateway));
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
            " Apagar ",
            &format!("Apagar '{}' e a senha salva?", app.store.hosts[*i].alias),
        ),
        Mode::ConfirmQuit => draw_confirm(
            f,
            " Sair ",
            &format!("Sair e fechar {} conexão(ões) abertas?", app.open_count()),
        ),
        Mode::Help => draw_help(f),
        _ => {}
    }
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.visible();
    let rows = visible.iter().map(|&i| {
        let h = &app.store.hosts[i];
        let key = if h.password.is_empty() {
            Span::raw("  ")
        } else {
            Span::raw(KEY_ICON)
        };
        Row::new(vec![
            Cell::from(kind_icon(h, app.has_session(&h.alias), false)),
            Cell::from(key),
            Cell::from(h.alias.clone()).bold(),
            Cell::from(h.user.clone()),
            Cell::from(h.hostname.clone()),
            Cell::from(h.port.to_string()),
            Cell::from(extra(h)).dim(),
        ])
    });

    let mut title = vec![Span::styled(" sshdeck ", Style::new().fg(ACCENT).bold())];
    title.push(Span::raw(format!("· {} hosts ", app.store.hosts.len())));
    if !app.filter.is_empty() || matches!(app.mode, Mode::Filter) {
        title.push(Span::styled(
            format!("· filtro: {}", app.filter),
            Style::new().fg(Color::Yellow),
        ));
        if matches!(app.mode, Mode::Filter) {
            title.push(Span::styled("▏", Style::new().fg(Color::Yellow)));
        }
        title.push(Span::raw(" "));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(12),
            Constraint::Min(10),
            Constraint::Min(15),
            Constraint::Length(6),
            Constraint::Min(10),
        ],
    )
    .header(
        Row::new(vec!["", "", "Alias", "User", "Host / IP", "Port", "Extra"])
            .style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
            .bottom_margin(1),
    )
    .row_highlight_style(
        Style::new()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ")
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .title(Line::from(title)),
    );

    if visible.is_empty() {
        let msg = if app.store.hosts.is_empty() {
            "Nenhum host ainda.\n\n'a' adiciona um host  ·  'i' importa do ~/.ssh/config"
        } else {
            "Nenhum host corresponde ao filtro."
        };
        f.render_widget(table, area);
        let inner = centered(area, 60, 5);
        f.render_widget(Paragraph::new(msg).centered().dim(), inner);
    } else {
        f.render_stateful_widget(table, area, &mut app.table);
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

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let [list_area, info_area] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(14)]).areas(area);

    let active_alias = app.active_session().map(|s| s.alias.clone());
    let visible = app.visible();
    let rows: Vec<Row> = visible
        .iter()
        .map(|&i| {
            let h = &app.store.hosts[i];
            let active = Some(&h.alias) == active_alias.as_ref();
            let key = if h.password.is_empty() { "" } else { KEY_ICON };
            Row::new(vec![
                Cell::from(kind_icon(h, app.has_session(&h.alias), active)),
                Cell::from(key),
                Cell::from(h.alias.clone()).bold(),
                Cell::from(h.hostname.clone()).dim(),
            ])
        })
        .collect();

    let mut title = vec![Span::styled(" Servidores ", Style::new().fg(ACCENT).bold())];
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
        .title(Span::styled(" Conexão ", Style::new().fg(ACCENT).bold()));
    let Some(s) = app.active_session() else {
        f.render_widget(block, area);
        return;
    };
    let field = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<7}"), Style::new().dark_gray()),
            Span::raw(v),
        ])
    };
    let mut lines = vec![Line::from(Span::styled(
        s.alias.clone(),
        Style::new().fg(Color::Green).bold(),
    ))];
    if let Some(h) = app.host_by_alias(&s.alias) {
        lines.push(field(
            "user",
            if h.user.is_empty() {
                "-".into()
            } else {
                h.user.clone()
            },
        ));
        lines.push(field("host", h.hostname.clone()));
        lines.push(field("porta", h.port.to_string()));
        if !h.identity_file.is_empty() {
            lines.push(field("chave", h.identity_file.clone()));
        }
        lines.push(field(
            "senha",
            if h.password.is_empty() {
                "não salva".into()
            } else {
                format!("{KEY_ICON} salva (Alt+P)")
            },
        ));
        lines.push(field("tempo", elapsed(s.started.elapsed().as_secs())));
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
            format!("histórico -{} ", screen.scrollback()),
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

/// Rodapé com os atalhos do contexto atual — sempre visível.
fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let keys: &[(&str, &str)] = match &app.mode {
        Mode::Password { purpose, .. } => &[
            ("Enter", "confirmar"),
            (
                "Esc",
                if *purpose == PwPurpose::Rekey {
                    "cancelar"
                } else {
                    "sair"
                },
            ),
        ],
        Mode::Form(form) if form.focused() == Field::Kind => &[
            ("←/→ ou espaço", "SSH/RDP"),
            ("Tab/↓", "próximo"),
            ("Ctrl+S", "salvar"),
            ("Esc", "cancelar"),
        ],
        Mode::Form(_) => &[
            ("Tab/↓", "próximo"),
            ("Shift+Tab/↑", "anterior"),
            ("Ctrl+S", "salvar"),
            ("Ctrl+R", "mostrar senha"),
            ("Ctrl+U", "limpar campo"),
            ("Esc", "cancelar"),
        ],
        Mode::ConfirmDelete(_) | Mode::ConfirmQuit => &[("s", "sim"), ("outra tecla", "não")],
        Mode::Help => &[("qualquer tecla", "fechar")],
        Mode::Filter => &[("digite", "filtrar"), ("Enter", "ok"), ("Esc", "limpar")],
        Mode::List if app.term_focus && !app.sessions.is_empty() => &[
            ("Ctrl+]", "painel"),
            ("Alt+P", "digitar senha"),
            ("Alt+←/→", "trocar sessão"),
            ("Shift+PgUp/PgDn", "histórico"),
        ],
        Mode::List if !app.sessions.is_empty() => &[
            ("Enter", "conectar"),
            ("Tab", "terminal"),
            ("x", "fechar sessão"),
            ("K", "ssh-copy-id"),
            ("c", "copiar senha"),
            ("a/e/d", "add/editar/apagar"),
            ("/", "filtrar"),
            ("?", "ajuda"),
            ("q", "sair"),
        ],
        Mode::List => &[
            ("Enter", "conectar"),
            ("x", "fechar"),
            ("s", "tela cheia"),
            ("K", "ssh-copy-id"),
            ("c", "copiar senha"),
            ("a/e/d", "add/editar/apagar"),
            ("i", "importar"),
            ("/", "filtrar"),
            ("?", "ajuda"),
            ("q", "sair"),
        ],
    };
    f.render_widget(Paragraph::new(hints(keys)), area);
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

fn hints(keys: &[(&str, &str)]) -> Line<'static> {
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
        PwPurpose::Unlock => " Desbloquear cofre ",
        PwPurpose::Create => " Criar cofre ",
        PwPurpose::Rekey => " Nova senha mestra ",
    };
    let prompt = match (purpose, confirming) {
        (PwPurpose::Unlock, _) => "Senha mestra:",
        (_, false) => "Defina a senha mestra:",
        (_, true) => "Confirme a senha mestra:",
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
        None if purpose == PwPurpose::Create => {
            lines.push(Line::from("Ela protege todas as senhas salvas. Não esqueça!").dim())
        }
        None => lines.push(Line::from("")),
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(title, Style::new().fg(ACCENT).bold()));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_form(f: &mut Frame, form: &Form) {
    let fields = form.fields();
    let area = centered(f.area(), 84, fields.len() as u16 + 5);
    f.render_widget(Clear, area);
    let title = if form.editing.is_some() {
        " Editar host "
    } else {
        " Novo host "
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
                spans.push(Span::raw(" ←/→ troca").dark_gray());
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
        let placeholder = match field {
            Field::Port => Some(port_hint.as_str()),
            Field::IdentityFile => Some(form.identity_hint.as_str()),
            Field::Domain => Some("vazio = conta local"),
            Field::Resolution => {
                Some("vazio = janela 80% redimensionável · full · 1920x1080 · 90%")
            }
            Field::Share => Some("vazio = nenhuma · ex.: ~/Downloads"),
            Field::Gateway => Some("vazio = conexão direta · ex.: alias de um host SSH"),
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
        .title(Span::styled(title, Style::new().fg(ACCENT).bold()));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_confirm(f: &mut Frame, title: &str, question: &str) {
    let area = centered(f.area(), (question.chars().count() as u16 + 6).max(40), 5);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Red))
        .title(title.to_string());
    let text = vec![
        Line::from(question.to_string()),
        Line::from(""),
        hints(&[("s", "sim"), ("outra tecla", "não")]),
    ];
    f.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_help(f: &mut Frame) {
    let rows = [
        (
            "Enter",
            "conecta: SSH no painel central, RDP em janela própria",
        ),
        ("s", "conecta em tela cheia (ssh normal)"),
        ("Tab / l", "vai para o terminal"),
        ("x", "fecha a sessão (ou a janela RDP) do host"),
        (
            "K",
            "ssh-copy-id: instala sua chave no servidor (usa a senha salva)",
        ),
        ("c / y", "copia a senha para o clipboard"),
        ("a", "adiciona host"),
        ("e", "edita host selecionado"),
        ("d / Del", "apaga host"),
        ("/", "filtra por alias, user ou host"),
        ("i", "importa hosts do ~/.ssh/config"),
        ("P", "troca a senha mestra"),
        ("j/k ↑/↓ g/G", "navegação"),
        ("q / Esc", "sair"),
        ("", ""),
        ("No terminal:", ""),
        ("Ctrl+]", "volta para o painel (sessão continua)"),
        ("Alt+P", "digita a senha salva no terminal"),
        ("Alt+← / →", "troca de sessão"),
        ("Shift+PgUp/Dn", "rola o histórico"),
        ("", ""),
        ("Dica", "sudo su → Alt+P (ou Ctrl+Shift+V) → Enter"),
    ];
    let area = centered(f.area(), 90, rows.len() as u16 + 4);
    f.render_widget(Clear, area);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("  {k:<16}"), Style::new().fg(ACCENT).bold()),
                Span::raw(*d),
            ])
        })
        .collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(" Ajuda ", Style::new().fg(ACCENT).bold()))
        .title_bottom(Line::from(" qualquer tecla fecha ").dim());
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
