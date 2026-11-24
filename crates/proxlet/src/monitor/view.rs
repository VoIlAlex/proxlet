//! The monitor's state and key handling, independent of the terminal
//! (design decision 8).

use std::cmp::Ordering;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::control::{StatsReply, StatsRow};

/// The column rows are sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// By route, ascending.
    #[default]
    Route,
    /// By requests per second, descending.
    Rps,
    /// By 4xx + 5xx, descending.
    Errors,
    /// By p95 latency, descending, rows without latency last.
    P95,
}

impl Sort {
    /// The next sort in the cycle.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Route => Self::Rps,
            Self::Rps => Self::Errors,
            Self::Errors => Self::P95,
            Self::P95 => Self::Route,
        }
    }

    /// The headers the sort marker goes on.
    #[must_use]
    pub fn columns(self) -> &'static [&'static str] {
        match self {
            Self::Route => &["RULE"],
            Self::Rps => &["REQ/S"],
            Self::Errors => &["4XX", "5XX"],
            Self::P95 => &["P95"],
        }
    }

    fn compare(self, a: &StatsRow, b: &StatsRow) -> Ordering {
        let by_route = || a.rule.cmp(&b.rule);
        match self {
            Self::Route => by_route(),
            Self::Rps => b.rps.total_cmp(&a.rps).then_with(by_route),
            Self::Errors => (b.status_4xx + b.status_5xx)
                .cmp(&(a.status_4xx + a.status_5xx))
                .then_with(by_route),
            Self::P95 => match (a.p95_ms, b.p95_ms) {
                (Some(x), Some(y)) => y.total_cmp(&x),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
            .then_with(by_route),
        }
    }
}

/// Whether the server answered the last time we asked.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// Not asked yet.
    Connecting,
    /// The latest statistics.
    Up(StatsReply),
    /// The server is not running; trying again each second.
    Down,
}

/// What to do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Keep going.
    Continue,
    /// Restore the terminal and exit.
    Quit,
}

/// Everything the live table shows.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// The server's state.
    pub link: Link,
    /// The current sort.
    pub sort: Sort,
    /// The first row shown.
    pub scroll: usize,
}

impl Default for View {
    fn default() -> Self {
        Self {
            link: Link::Connecting,
            sort: Sort::default(),
            scroll: 0,
        }
    }
}

impl View {
    /// Takes the result of asking for statistics.
    pub fn update(&mut self, stats: Option<StatsReply>) {
        self.link = match stats {
            Some(s) => Link::Up(s),
            None => Link::Down,
        };
        self.scroll = self.scroll.min(self.rows().len().saturating_sub(1));
    }

    /// The rows in display order: sorted rules, then the "no rule" row.
    #[must_use]
    pub fn rows(&self) -> Vec<&StatsRow> {
        let Link::Up(stats) = &self.link else {
            return Vec::new();
        };
        let (mut rules, rest): (Vec<&StatsRow>, Vec<&StatsRow>) =
            stats.rows.iter().partition(|r| r.rule.is_some());
        rules.sort_by(|a, b| self.sort.compare(a, b));
        rules.extend(rest);
        rules
    }

    /// Handles a key; `visible` is how many rows fit on the screen.
    pub fn key(&mut self, key: KeyEvent, visible: usize) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::Continue;
        }
        let last = self.rows().len().saturating_sub(visible.max(1));
        let page = visible.max(1);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Action::Quit;
            }
            KeyCode::Char('s') => self.sort = self.sort.next(),
            KeyCode::Down => self.scroll = (self.scroll + 1).min(last),
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = (self.scroll + page).min(last),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            _ => {}
        }
        Action::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(rule: Option<&str>, rps: f64, errors: u64, p95: Option<f64>) -> StatsRow {
        StatsRow {
            rule: rule.map(str::to_owned),
            rps,
            status_5xx: errors,
            p95_ms: p95,
            total: 7,
            ..StatsRow::default()
        }
    }

    fn view(rows: Vec<StatsRow>) -> View {
        let mut v = View::default();
        v.update(Some(StatsReply {
            uptime_secs: 1,
            connections: 0,
            rows,
        }));
        v
    }

    fn order(v: &View) -> Vec<Option<&str>> {
        v.rows().iter().map(|r| r.rule.as_deref()).collect()
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn sorts_cycle_with_no_rule_last() {
        let mut v = view(vec![
            row(Some("/a"), 1.0, 5, None),
            row(Some("/b"), 9.0, 0, Some(10.0)),
            row(Some("/c"), 3.0, 2, Some(90.0)),
            row(None, 50.0, 99, Some(999.0)),
        ]);
        assert_eq!(order(&v), [Some("/a"), Some("/b"), Some("/c"), None]);
        v.key(press(KeyCode::Char('s')), 10);
        assert_eq!(v.sort, Sort::Rps);
        assert_eq!(order(&v), [Some("/b"), Some("/c"), Some("/a"), None]);
        v.key(press(KeyCode::Char('s')), 10);
        assert_eq!(order(&v), [Some("/a"), Some("/c"), Some("/b"), None]);
        v.key(press(KeyCode::Char('s')), 10);
        // Without latency last.
        assert_eq!(order(&v), [Some("/c"), Some("/b"), Some("/a"), None]);
        v.key(press(KeyCode::Char('s')), 10);
        assert_eq!(v.sort, Sort::Route);
    }

    #[test]
    fn scrolling_stays_in_bounds() {
        let rows = (0..10)
            .map(|i| row(Some(&format!("/r{i}")), 0.0, 0, None))
            .collect();
        let mut v = view(rows);
        v.key(press(KeyCode::Up), 4);
        assert_eq!(v.scroll, 0);
        for _ in 0..20 {
            v.key(press(KeyCode::Down), 4);
        }
        assert_eq!(v.scroll, 6, "10 rows, 4 visible");
        v.key(press(KeyCode::PageUp), 4);
        assert_eq!(v.scroll, 2);
        v.key(press(KeyCode::PageDown), 4);
        assert_eq!(v.scroll, 6);
        // Fewer rows after an update: the scroll follows.
        v.update(Some(StatsReply::default()));
        assert_eq!(v.scroll, 0);
    }

    #[test]
    fn quit_keys() {
        let mut v = View::default();
        assert_eq!(v.key(press(KeyCode::Char('q')), 1), Action::Quit);
        assert_eq!(v.key(press(KeyCode::Esc), 1), Action::Quit);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(v.key(ctrl_c, 1), Action::Quit);
        assert_eq!(v.key(press(KeyCode::Char('c')), 1), Action::Continue);
        assert_eq!(v.key(press(KeyCode::Char('x')), 1), Action::Continue);
    }

    /// A stats source that answers, fails while the server restarts, and
    /// answers again with fresh totals.
    #[test]
    fn restart_while_watching() {
        let answers = [
            Some(StatsReply {
                uptime_secs: 100,
                connections: 1,
                rows: vec![row(Some("/api"), 1.0, 0, None)],
            }),
            None,
            None,
            Some(StatsReply {
                uptime_secs: 1,
                connections: 0,
                rows: vec![StatsRow {
                    rule: Some("/api".into()),
                    ..StatsRow::default()
                }],
            }),
        ];
        let mut v = View::default();
        assert_eq!(v.link, Link::Connecting);
        let mut seen = Vec::new();
        for a in answers {
            v.update(a);
            seen.push(match &v.link {
                Link::Up(s) => format!("up {}", s.rows[0].total),
                Link::Down => "down".to_owned(),
                Link::Connecting => "connecting".to_owned(),
            });
            if v.link == Link::Down {
                assert!(v.rows().is_empty(), "no stale rows while down");
            }
        }
        assert_eq!(seen, ["up 7", "down", "down", "up 0"]);
    }
}
