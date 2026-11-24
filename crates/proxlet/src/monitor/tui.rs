//! Drawing the live table (design decision 7).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use super::format::{self, HEADERS, LEFT_ALIGNED};
use super::view::{Link, View};

/// Lines above the table rows: the status line and the column headers.
const CHROME: u16 = 2;

/// How many rows fit in an area of this height.
#[must_use]
pub fn visible_rows(height: u16) -> usize {
    usize::from(height.saturating_sub(CHROME))
}

/// Draws the whole screen.
pub fn draw(frame: &mut Frame, view: &View) {
    let [status, table] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(frame.area());
    frame.render_widget(Paragraph::new(status_line(view)), status);
    match &view.link {
        Link::Up(_) => draw_table(frame, table, view),
        Link::Connecting => {
            frame.render_widget(Paragraph::new("Connecting to proxlet…"), table);
        }
        Link::Down => frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "proxlet is not running",
                    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" — reconnecting… (start it with: proxlet start)"),
            ])),
            table,
        ),
    }
}

fn status_line(view: &View) -> Line<'static> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut spans = vec![Span::styled(
        "proxlet monitor",
        Style::new().add_modifier(Modifier::BOLD),
    )];
    if let Link::Up(s) = &view.link {
        let conns = if s.connections == 1 {
            "1 connection".to_owned()
        } else {
            format!("{} connections", s.connections)
        };
        spans.push(Span::raw(format!(
            "  up {}  {conns}",
            format::uptime(s.uptime_secs)
        )));
    }
    spans.push(Span::styled("  s sort  ↑↓ scroll  q quit", dim));
    Line::from(spans)
}

fn draw_table(frame: &mut Frame, area: Rect, view: &View) {
    let rows = view.rows();
    let cells: Vec<[String; 13]> = rows.iter().map(|r| format::cells(r)).collect();
    let marked = view.sort.columns();
    let headers: Vec<String> = HEADERS
        .iter()
        .map(|h| {
            if marked.contains(h) {
                format!("{h}▼")
            } else {
                (*h).to_owned()
            }
        })
        .collect();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in &cells {
        for (w, c) in widths.iter_mut().zip(row) {
            *w = (*w).max(c.chars().count());
        }
    }
    let align = |i: usize, text: String| {
        let line = Line::from(text);
        Cell::from(if i < LEFT_ALIGNED {
            line
        } else {
            line.right_aligned()
        })
    };
    let header = Row::new(headers.into_iter().enumerate().map(|(i, h)| align(i, h)))
        .style(Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED));
    let body = rows
        .iter()
        .zip(cells)
        .skip(view.scroll)
        .map(|(row, cells)| {
            let style = if row.rule.is_none() {
                Style::new().add_modifier(Modifier::DIM)
            } else if row.status_5xx > 0 {
                Style::new().fg(Color::Red)
            } else if row.status_4xx > 0 {
                Style::new().fg(Color::Yellow)
            } else {
                Style::new()
            };
            Row::new(cells.into_iter().enumerate().map(|(i, c)| align(i, c))).style(style)
        });
    let constraints = widths
        .into_iter()
        .map(|w| Constraint::Length(u16::try_from(w).unwrap_or(u16::MAX)));
    frame.render_widget(
        Table::new(body, constraints)
            .header(header)
            .column_spacing(2),
        area,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::control::{StatsReply, StatsRow};
    use crate::monitor::view::Sort;

    fn screen(view: &View, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, view)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..height {
            let mut line = String::new();
            for x in 0..width {
                line.push_str(buffer[(x, y)].symbol());
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    fn up() -> View {
        let mut v = View::default();
        v.update(Some(StatsReply {
            uptime_secs: 725,
            connections: 2,
            rows: vec![
                StatsRow {
                    rule: Some("/api".into()),
                    target: Some("127.0.0.1:8000".into()),
                    rps: 4.2,
                    total: 512,
                    p95_ms: Some(40.0),
                    ..StatsRow::default()
                },
                StatsRow::default(),
            ],
        }));
        v
    }

    #[test]
    fn table_with_status_line() {
        let s = screen(&up(), 110, 5);
        let lines: Vec<&str> = s.lines().collect();
        assert!(
            lines[0].starts_with("proxlet monitor  up 12m 5s  2 connections"),
            "{s}"
        );
        assert!(lines[1].starts_with("RULE▼"), "{s}");
        assert!(lines[1].contains("UP-ERR"), "{s}");
        assert!(lines[2].starts_with("/api"), "{s}");
        assert!(lines[2].contains("127.0.0.1:8000"), "{s}");
        assert!(lines[2].contains("40ms"), "{s}");
        assert!(lines[3].starts_with("(no rule)"), "{s}");
    }

    #[test]
    fn sort_marker_moves() {
        let mut v = up();
        v.sort = Sort::P95;
        let s = screen(&v, 110, 5);
        assert!(s.lines().nth(1).unwrap().contains("P95▼"), "{s}");
        assert!(!s.contains("RULE▼"), "{s}");
    }

    #[test]
    fn not_running_banner() {
        let mut v = up();
        v.update(None);
        let s = screen(&v, 110, 5);
        assert!(s.contains("proxlet is not running — reconnecting…"), "{s}");
        assert!(!s.contains("/api"), "{s}");
    }

    #[test]
    fn visible_rows_leave_room_for_the_chrome() {
        assert_eq!(visible_rows(10), 8);
        assert_eq!(visible_rows(1), 0);
    }
}
