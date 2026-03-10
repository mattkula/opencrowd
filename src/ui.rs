use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, InputMode};

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(f.area());

    let main_area = chunks[0];
    let footer_area = chunks[1];

    // Split main area: feature list on top, detail panel below
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(9),
        ])
        .split(main_area);

    draw_feature_list(f, app, main_chunks[0]);
    draw_detail_panel(f, app, main_chunks[1]);
    draw_footer(f, app, footer_area);

    // Draw input overlay if in input mode
    if app.input_mode == InputMode::CreatingFeature {
        draw_input_dialog(f, app);
    }
}

fn draw_feature_list(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .state
        .features
        .iter()
        .enumerate()
        .map(|(i, feature)| {
            let status_color = feature.status.color();
            let symbol = feature.status.symbol(app.spinner_frame);
            let is_active = app.active_feature.as_ref() == Some(&feature.name);

            let selected = i == app.selected_index;
            let name_style = if is_active {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            // Show an arrow for the active feature (the one in the right panes)
            let active_indicator = if is_active { " <" } else { "" };

            let line = Line::from(vec![
                Span::styled(
                    format!(" [{}] ", symbol),
                    Style::default().fg(status_color),
                ),
                Span::styled(&feature.name, name_style),
                Span::styled(
                    format!("  {}", feature.branch),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    active_indicator,
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
            ]);

            ListItem::new(line)
        })
        .collect();

    let feature_count = app.state.features.len();
    let title = format!(" Features ({}) ", feature_count);

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(30, 30, 30)),
        )
        .highlight_symbol(">");

    let mut list_state = ListState::default();
    if !app.state.features.is_empty() {
        list_state.select(Some(app.selected_index));
    }

    f.render_stateful_widget(list, area, &mut list_state);
}

fn draw_detail_panel(f: &mut Frame, app: &App, area: Rect) {
    if let Some(feature) = app.selected_feature() {
        let status_color = feature.status.color();
        let is_active = app.active_feature.as_ref() == Some(&feature.name);

        let lines = vec![
            Line::from(vec![
                Span::styled(" Name:     ", Style::default().fg(Color::DarkGray)),
                Span::styled(&feature.name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                if is_active {
                    Span::styled("  [showing in panes]", Style::default().fg(Color::Green))
                } else {
                    Span::styled("  [press Enter to open]", Style::default().fg(Color::DarkGray))
                },
            ]),
            Line::from(vec![
                Span::styled(" Branch:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(&feature.branch, Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::styled(" Path:     ", Style::default().fg(Color::DarkGray)),
                Span::styled(&feature.worktree_path, Style::default().fg(Color::Gray)),
            ]),
            Line::from(vec![
                Span::styled(" Status:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("[{}] {}", feature.status.symbol(app.spinner_frame), feature.status),
                    Style::default().fg(status_color),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Created:  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    feature.created_at.format("%Y-%m-%d %H:%M").to_string(),
                    Style::default().fg(Color::Gray),
                ),
            ]),
        ];

        let detail = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Details ")
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .wrap(Wrap { trim: false });

        f.render_widget(detail, area);
    } else {
        let empty = Paragraph::new(vec![
            Line::from(""),
            Line::from("  No features yet. Press 'n' to create one."),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Details ")
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .style(Style::default().fg(Color::DarkGray));

        f.render_widget(empty, area);
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let msg = match &app.input_mode {
        InputMode::Normal => {
            vec![
                Span::styled(" n", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(" new  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Enter", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(" open  ", Style::default().fg(Color::DarkGray)),
                Span::styled("d", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(" delete  ", Style::default().fg(Color::DarkGray)),
                Span::styled("q", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(" detach  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Q", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(" kill all", Style::default().fg(Color::DarkGray)),
            ]
        }
        InputMode::CreatingFeature => {
            vec![
                Span::styled("Enter", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(" confirm  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Esc", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
            ]
        }
        InputMode::ConfirmDelete | InputMode::ConfirmDeleteBranch => {
            vec![
                Span::styled("y", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::styled(" yes  ", Style::default().fg(Color::DarkGray)),
                Span::styled("n", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(" no", Style::default().fg(Color::DarkGray)),
            ]
        }
    };

    let footer_content = if let Some(status) = &app.status_message {
        let mut spans = vec![
            Span::styled(
                format!(" {} ", status),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(" | "),
        ];
        spans.extend(msg);
        spans
    } else {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(msg);
        spans
    };

    let footer = Paragraph::new(Line::from(footer_content))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

    f.render_widget(footer, area);
}

fn draw_input_dialog(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 30, f.area());

    f.render_widget(Clear, area);

    let input_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .margin(1)
        .split(area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" New Feature ")
        .border_style(Style::default().fg(Color::Cyan));

    f.render_widget(block, area);

    let label = Paragraph::new("Feature name:")
        .style(Style::default().fg(Color::Gray));
    f.render_widget(label, input_chunks[0]);

    let input = Paragraph::new(app.input_buffer.as_str())
        .style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    f.render_widget(input, input_chunks[1]);

    // Place cursor
    f.set_cursor_position((
        input_chunks[1].x + app.input_buffer.len() as u16 + 1,
        input_chunks[1].y + 1,
    ));
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
