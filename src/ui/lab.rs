//! Checklist panel for `rtop lab <name>`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::{panel, theme};
use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let Some(lab) = &app.lab else { return };
    let step = lab.current();
    let mut lines = vec![Line::from(Span::styled(lab.lab.intro, theme::dim()))];
    lines.push(Line::from(vec![
        Span::styled(format!("Step {}/{}  ", lab.step + 1, lab.lab.steps.len()), theme::title()),
        Span::styled(step.task, Style::default().add_modifier(Modifier::BOLD)),
    ]));
    if lab.revealed {
        lines.push(Line::from(vec![
            Span::styled("Answer  ", Style::default().fg(theme::pal().good).add_modifier(Modifier::BOLD)),
            Span::raw(step.answer),
        ]));
    } else {
        lines.push(Line::from(Span::styled("v reveal answer · n next step · b previous", theme::dim())));
    }
    if let Some(note) = &lab.exit_note {
        lines.push(Line::from(Span::styled(format!("● {note}"), Style::default().fg(theme::pal().warn))));
    }
    let p = Paragraph::new(lines).block(panel(format!("lab · {}", lab.lab.title))).wrap(Wrap { trim: true });
    f.render_widget(p, area);
}
