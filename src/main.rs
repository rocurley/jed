use argh::FromArgs;
use regex::Regex;
use std::{fs, io};
use termion::{
    event::Key,
    input::{MouseTerminal, TermRead},
    raw::IntoRawMode,
    screen::AlternateScreen,
};
use tui::{
    backend::TermionBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use unicode_width::UnicodeWidthStr;

#[cfg(feature = "dev-tools")]
use cpuprofiler::PROFILER;
use jed::{
    cursor::{Cursor, FocusPosition},
    view_tree::{View, ViewFrame, ViewTree, ViewTreeIndex},
};
#[cfg(feature = "dev-tools")]
use prettytable::{cell, ptable, row, table, Table};

#[derive(FromArgs, PartialEq, Debug)]
/// Json viewer and editor
struct Args {
    #[cfg(feature = "dev-tools")]
    #[argh(subcommand)]
    mode: Mode,
    #[argh(positional)]
    json_path: String,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum Mode {
    Normal(NormalMode),
    Bench(BenchMode),
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "load")]
/// Run the editor
struct NormalMode {}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "bench")]
/// Benchmark loading a json file
struct BenchMode {}

// Large file perf (181 mb):
// * Old: 13.68 sec
//   * Initial parsing (serde): 3.77 sec
//   * Pre-rendering (lines): 2.29 sec (left and right)
//   * Query execution: 7.62 sec
//     * Serde -> JV: 3.38 sec
//     * Computing result: 0???? (it is the trivial filter)
//     * JV -> Serde: 3.37 sec
// * New: 6.32 sec
//   * Initial parsing (JV deserialize): 6.26
//   * Query execution: ~0
//
// What can we do to improve load times? The current situation looks bleak.
// * If (big if) JV iterated through maps in insertion order, you could imagine rendinering the
// scene before the file is fully loaded. We can't load instantly, but we can definitely load one
// page of json instantly. Probably worth reading the JV object implementation: hopefully it's not
// too complicated.
// * We might be able to deserialize in parallel.
// * Use private JV functions to bypass typechecking when we already know the type.
// * Only use JVRaws duing deserialization.
// * Stop using JQ entirely (this would be hellish)
// * If you can guarantee identiacal rendering from JV and serde Values, deserialize into a serde
// Value (faster), become interactive then, and secretly swap in the JV once that's ready. Not
// great from a memory perspective. Any way to do that incrementally? Since we'd have full control
// over the value-like structure, it might be doable. Shared mutable access across different
// threads is.... a concenrn.
// * Completely violate the JV privacy boundary and construct JVs directly. Would we be able to
// make it faster? I'd be surprised: my guess is that the JV implementation is fairly optimal
// _given_ the datastructure, which we wouldn't be able to avoid.
// * Write an interpreter for JQ bytecode. That's definitely considered an implementation detail,
// so that would be pretty evil, but we might be able to operate directly on serde Values.
//
// TODO
// * Long strings
// * Edit tree, instead of 2 fixed panels
// * Saving
// * Cleanup
//   * Cursor is kind of messy
#[cfg(feature = "dev-tools")]
fn main() -> Result<(), io::Error> {
    let args: Args = argh::from_env();
    match args.mode {
        Mode::Normal(_) => run(args.json_path),
        Mode::Bench(_) => bench(args.json_path),
    }
}

#[cfg(not(feature = "dev-tools"))]
fn main() -> Result<(), io::Error> {
    let args: Args = argh::from_env();
    run(args.json_path)
}

fn force_draw<B: tui::backend::Backend, F: FnMut(&mut Frame<B>)>(
    terminal: &mut Terminal<B>,
    mut f: F,
) -> Result<(), io::Error> {
    terminal.autoresize()?;
    let mut frame = terminal.get_frame();
    f(&mut frame);
    let current_buffer = terminal.current_buffer_mut().clone();
    terminal.current_buffer_mut().reset();
    terminal.draw(f)?;
    let area = current_buffer.area;
    let width = area.width;

    let mut updates: Vec<(u16, u16, &tui::buffer::Cell)> = vec![];
    // Cells from the current buffer to skip due to preceeding multi-width characters taking their
    // place (the skipped cells should be blank anyway):
    let mut to_skip: usize = 0;
    for (i, current) in current_buffer.content.iter().enumerate() {
        if to_skip == 0 {
            let x = i as u16 % width;
            let y = i as u16 / width;
            updates.push((x, y, &current_buffer.content[i]));
        }

        to_skip = current.symbol.width().saturating_sub(1);
    }
    terminal.backend_mut().draw(updates.into_iter())
}

fn run(json_path: String) -> Result<(), io::Error> {
    let stdin = io::stdin();
    let f = fs::File::open(&json_path)?;
    let r = io::BufReader::new(f);
    let mut app = App::new(r, json_path)?;
    let stdout = io::stdout().into_raw_mode()?;
    let stdout = MouseTerminal::from(stdout);
    let stdout = AlternateScreen::from(stdout);
    let backend = TermionBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(app.render(AppRenderMode::Normal))?;
    let mut query_rl: rustyline::Editor<()> = rustyline::Editor::new();
    let mut search_rl: rustyline::Editor<()> = rustyline::Editor::new();
    query_rl.bind_sequence(rustyline::KeyPress::Esc, rustyline::Cmd::Interrupt);
    search_rl.bind_sequence(rustyline::KeyPress::Esc, rustyline::Cmd::Interrupt);
    for c in stdin.keys() {
        let c = c?;
        match c {
            Key::Esc => break,
            Key::Char('t') => {
                app.show_tree = !app.show_tree;
            }
            Key::Char('q') => {
                terminal.draw(app.render(AppRenderMode::QueryEditor))?;
                let (_, _, query) = app.current_views_mut();
                match query_rl.readline_with_initial("", (&*query, "")) {
                    Ok(new_query) => {
                        *query = new_query;
                        // Just in case rustyline messed stuff up
                        force_draw(&mut terminal, app.render(AppRenderMode::Normal))?;
                        app.recompute_right();
                    }
                    Err(_) => {}
                }
            }
            Key::Char('\t') => app.focus = app.focus.swap(),
            _ => {}
        }
        let layout = JedLayout::new(&terminal.get_frame(), app.show_tree);
        let view_rect = match app.focus {
            Focus::Left => layout.left,
            Focus::Right => layout.right,
        };
        let view = app.focused_view_mut();
        let line_limit = view_rect.height as usize - 2;
        match &mut view.view {
            View::Error(_) => {}
            View::Json(None) => {}
            View::Json(Some(view)) => match c {
                Key::Down => {
                    view.cursor.advance(&view.folds);
                    if !view
                        .visible_range(&view.folds, line_limit)
                        .contains(&view.cursor.to_path())
                    {
                        view.scroll.advance(&view.folds);
                    }
                }
                Key::Up => {
                    view.cursor.regress(&view.folds);
                    if !view
                        .visible_range(&view.folds, line_limit)
                        .contains(&view.cursor.to_path())
                    {
                        view.scroll.regress(&view.folds);
                    }
                }
                Key::Char('z') => {
                    let path = view.cursor.to_path().strip_position();
                    if view.folds.contains(&path) {
                        view.folds.remove(&path);
                    } else {
                        view.folds.insert(path);
                        if let FocusPosition::End = view.cursor.focus_position {
                            view.cursor.focus_position = FocusPosition::Start;
                        }
                    }
                }
                Key::Char('/') => {
                    terminal.draw(app.render(AppRenderMode::SearchEditor))?;
                    match search_rl.readline_with_initial("Search:", ("", "")) {
                        Ok(new_search) => {
                            // Just in case rustyline messed stuff up
                            force_draw(&mut terminal, app.render(AppRenderMode::Normal))?;
                            app.search_re = Regex::new(new_search.as_ref()).ok();
                            app.search(line_limit, false);
                        }
                        Err(_) => {}
                    }
                }
                Key::Char('n') => {
                    app.search(line_limit, false);
                }
                Key::Char('N') => {
                    app.search(line_limit, true);
                }
                Key::Home => {
                    view.cursor =
                        Cursor::new(view.values.clone()).expect("values should still exist");
                    view.scroll = view.cursor.clone();
                }
                Key::End => {
                    view.cursor =
                        Cursor::new_end(view.values.clone()).expect("values should still exist");
                    view.scroll = view.cursor.clone();
                    for _ in 0..line_limit - 1 {
                        view.scroll.regress(&view.folds);
                    }
                }
                _ => {}
            },
        }
        terminal.draw(app.render(AppRenderMode::Normal))?;
    }
    // Gracefully freeing the JV values can take a significant amount of time and doesn't actually
    // benefit anything: the OS will clean up after us when we exit.
    std::mem::forget(app);
    Ok(())
}

#[cfg(feature = "dev-tools")]
fn bench(json_path: String) -> Result<(), io::Error> {
    let mut profiler = PROFILER.lock().unwrap();
    profiler.start("profile").unwrap();
    let f = fs::File::open(json_path)?;
    let r = io::BufReader::new(f);
    let app = App::new(r)?;
    std::mem::forget(app);
    profiler.stop().unwrap();
    Ok(())
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum Focus {
    Left,
    Right,
}

impl Focus {
    fn swap(self) -> Self {
        match self {
            Focus::Left => Focus::Right,
            Focus::Right => Focus::Left,
        }
    }
}

struct App {
    views: ViewTree,
    index: ViewTreeIndex,
    focus: Focus,
    search_re: Option<Regex>,
    show_tree: bool,
}

struct JedLayout {
    tree: Option<Rect>,
    left: Rect,
    right: Rect,
    query: Rect,
}

impl JedLayout {
    fn new<B: tui::backend::Backend>(f: &Frame<B>, show_tree: bool) -> JedLayout {
        let size = f.size();
        let vchunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)].as_ref())
            .split(size);
        if show_tree {
            let tree_split = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(20), Constraint::Ratio(1, 1)].as_ref())
                .split(vchunks[0]);
            let views = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)].as_ref())
                .split(tree_split[1]);
            JedLayout {
                tree: Some(tree_split[0]),
                left: views[0],
                right: views[1],
                query: vchunks[1],
            }
        } else {
            let views = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)].as_ref())
                .split(vchunks[0]);
            JedLayout {
                tree: None,
                left: views[0],
                right: views[1],
                query: vchunks[1],
            }
        }
    }
}

enum AppRenderMode {
    Normal,
    QueryEditor,
    SearchEditor,
}

impl App {
    fn new<R: io::Read>(r: R, name: String) -> io::Result<Self> {
        let views = ViewTree::new_from_reader(r, name)?;
        let index = ViewTreeIndex {
            parent: Vec::new(),
            child: 0,
        };
        let app = App {
            views,
            index,
            focus: Focus::Left,
            search_re: None,
            show_tree: false,
        };
        Ok(app)
    }
    fn current_views(&self) -> (&ViewFrame, &ViewFrame, &String) {
        self.views
            .index(&self.index)
            .expect("App index invalidated")
    }
    fn current_views_mut(&mut self) -> (&mut ViewFrame, &mut ViewFrame, &mut String) {
        self.views
            .index_mut(&self.index)
            .expect("App index invalidated")
    }
    fn focused_view(&self) -> &ViewFrame {
        let (left, right, _) = self.current_views();
        match self.focus {
            Focus::Left => left,
            Focus::Right => right,
        }
    }
    fn focused_view_mut(&mut self) -> &mut ViewFrame {
        let focus = self.focus;
        let (left, right, _) = self.current_views_mut();
        match focus {
            Focus::Left => left,
            Focus::Right => right,
        }
    }
    fn recompute_right(&mut self) {
        let (left, right, query) = self.current_views_mut();
        match &mut left.view {
            View::Json(Some(left)) => {
                right.view = left.apply_query(query);
            }
            View::Json(None) | View::Error(_) => {
                right.view = View::Json(None);
            }
        }
    }
    fn render<B: tui::backend::Backend>(
        &self,
        mode: AppRenderMode,
    ) -> impl FnMut(&mut Frame<B>) + '_ {
        let App { focus, .. } = self;
        let (left, right, query) = self.current_views();
        move |f| {
            let layout = JedLayout::new(f, self.show_tree);
            let left_block = Block::default()
                .title(left.name.to_owned())
                .borders(Borders::ALL);
            let left_paragraph = left
                .view
                .render(layout.left.height, *focus == Focus::Left)
                .block(left_block);
            f.render_widget(left_paragraph, layout.left);
            let right_block = Block::default()
                .title(right.name.to_owned())
                .borders(Borders::ALL);
            let right_paragraph = right
                .view
                .render(layout.right.height, *focus == Focus::Right)
                .block(right_block);
            f.render_widget(right_paragraph, layout.right);
            if let Some(tree_rect) = layout.tree {
                let tree_block = Block::default().borders(Borders::ALL);
                f.render_widget(self.views.render_tree().block(tree_block), tree_rect);
            }
            match mode {
                AppRenderMode::Normal => {
                    let query = Paragraph::new(query.as_str())
                        .alignment(Alignment::Left)
                        .wrap(Wrap { trim: false });
                    f.render_widget(query, layout.query);
                }
                AppRenderMode::QueryEditor | AppRenderMode::SearchEditor => {
                    f.set_cursor(0, layout.query.y);
                }
            }
        }
    }
    fn search(&mut self, line_limit: usize, reverse: bool) {
        let re = if let Some(re) = &self.search_re {
            re
        } else {
            return;
        };
        let (left, right, _) = self
            .views
            .index_mut(&self.index)
            .expect("App index invalidated");
        let view = match self.focus {
            Focus::Left => left,
            Focus::Right => right,
        };
        let view = if let View::Json(Some(view)) = &mut view.view {
            view
        } else {
            return;
        };
        let search_hit = if reverse {
            view.cursor.clone().search_back(re)
        } else {
            view.cursor.clone().search(re)
        };
        if let Some(search_hit) = search_hit {
            view.cursor = search_hit;
        } else {
            return;
        };
        view.unfold_around_cursor();
        if !view
            .visible_range(&view.folds, line_limit)
            .contains(&view.cursor.to_path())
        {
            view.scroll = view.cursor.clone();
        }
    }
}
