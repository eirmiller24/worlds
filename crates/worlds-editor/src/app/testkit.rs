//! Test helpers for the panes: the mini fixture in an `EditorState`, and
//! an egui driver with clicks and keys.

use super::EditorState;
use crate::doc::Document;

/// The mini fixture in an `EditorState` (crevasse active).
pub fn state() -> (tempfile::TempDir, EditorState) {
    fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let p = e.unwrap().path();
            let to = to.join(p.file_name().unwrap());
            if p.is_dir() {
                copy_dir(&p, &to);
            } else {
                std::fs::copy(&p, &to).unwrap();
            }
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("mini.worlds");
    copy_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../worlds-core/tests/fixtures/mini.worlds"),
        &dir,
    );
    let doc = Document::open(&dir).unwrap();
    let mut view = crate::view::ViewState::new();
    view.sync_layers(&doc, None);
    view.active_level = doc.level_ix("crevasse");
    let st = EditorState {
        doc: Some(doc),
        view,
        ..Default::default()
    };
    (tmp, st)
}

/// Runs a pane in frames of its own egui context.
pub struct Driver {
    pub ctx: egui::Context,
    t: f64,
    pane: fn(&mut egui::Ui, &mut EditorState),
}

impl Driver {
    pub fn new(pane: fn(&mut egui::Ui, &mut EditorState)) -> Self {
        Driver {
            ctx: egui::Context::default(),
            t: 0.0,
            pane,
        }
    }

    pub fn frame(&mut self, st: &mut EditorState, events: Vec<egui::Event>) {
        self.t += 0.1;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 1400.0),
            )),
            time: Some(self.t),
            focused: true,
            events,
            ..Default::default()
        };
        let pane = self.pane;
        let mut out = self.ctx.run_ui(raw, |ui| pane(ui, st));
        out.textures_delta.clear();
    }

    pub fn click(&mut self, st: &mut EditorState, p: egui::Pos2) {
        for pressed in [true, false] {
            self.frame(
                st,
                vec![
                    egui::Event::PointerMoved(p),
                    egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
    }

    /// Type `text` into the focused field, then Enter.
    pub fn type_enter(&mut self, st: &mut EditorState, text: &str) {
        self.frame(st, vec![egui::Event::Text(text.into())]);
        let key = |pressed| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        };
        self.frame(st, vec![key(true), key(false)]);
    }

    pub fn rect(&self, key: &str) -> egui::Rect {
        super::fields::marked(&self.ctx, key).unwrap_or_else(|| panic!("nothing drawn for {key}"))
    }
}

/// The queued batches: (label, commands).
pub fn batches(st: &mut EditorState) -> Vec<(String, Vec<worlds_core::Command>)> {
    std::mem::take(&mut st.pending)
        .into_iter()
        .map(|c| match c {
            worlds_core::Command::Batch { label, commands } => (label, commands),
            other => (other.label(), vec![other]),
        })
        .collect()
}
