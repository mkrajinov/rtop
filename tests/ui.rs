//! Snapshot tests of whole screens, rendered from the fixture collector so
//! they never depend on what is running on the test machine.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rtop::app::App;
use rtop::collector::fixture::FixtureCollector;
use rtop::ui;

fn app() -> App {
    let mut app = App::new(Box::new(FixtureCollector::default()));
    app.refresh();
    app
}

fn render(app: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::draw(f, app)).unwrap();
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::from(code));
}

#[test]
fn process_table() {
    let mut app = app();
    insta::assert_snapshot!(render(&mut app, 130, 16));
}

#[test]
fn tree_view_shows_parentage() {
    let mut app = app();
    press(&mut app, KeyCode::Char('t'));
    let screen = render(&mut app, 130, 16);
    assert!(screen.contains("└─ "), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn explain_panel_follows_column_focus() {
    let mut app = app();
    press(&mut app, KeyCode::Char('e'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Right); // CPU% → MEM% → VSZ → RSS
    }
    let screen = render(&mut app, 150, 40);
    assert!(screen.contains("RSS — resident set size"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn detail_view_and_raw_jump() {
    let mut app = app();
    press(&mut app, KeyCode::Enter);
    let screen = render(&mut app, 120, 20);
    assert!(screen.contains("1 Overview"), "{screen}");
    insta::assert_snapshot!("detail_overview", screen);

    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down); // State
    press(&mut app, KeyCode::Char('r'));
    let screen = render(&mut app, 120, 20);
    insta::assert_snapshot!("detail_raw", screen);
}

#[test]
fn filter_narrows_rows() {
    let mut app = app();
    press(&mut app, KeyCode::Char('/'));
    for c in "hog".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.rows.len(), 1);
    assert_eq!(app.selected_process().unwrap().name, "cpuhog");
}

#[test]
fn theme_picker_previews_and_esc_restores() {
    let mut app = app();
    let before = rtop::ui::theme::current();
    press(&mut app, KeyCode::Char('T'));
    let screen = render(&mut app, 130, 30);
    assert!(screen.contains("neutral") && screen.contains("emerald"), "{screen}");
    press(&mut app, KeyCode::Down);
    assert_ne!(rtop::ui::theme::current(), before, "arrow keys apply live");
    press(&mut app, KeyCode::Esc);
    assert_eq!(rtop::ui::theme::current(), before);
}
