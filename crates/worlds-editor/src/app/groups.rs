//! Object groups in the editor (`level.json` `groups`): a click with
//! Select on a member selects the whole group, a double click enters the
//! group so the next clicks pick one member, Ctrl+G groups the selection
//! and Ctrl+Shift+G ungroups it, and the Inspector shows the group. The
//! commands come from [`worlds_core::command::groups`], shared with the
//! API and MCP.

use worlds_core::command::groups as core_groups;
use worlds_core::{Command, Id, ObjectGroup};

use super::EditorState;
use super::fields::mark;
use crate::doc::{Document, LevelIx};
use crate::view::{SelItem, ViewState};

/// The level holding a selected entity or annotation (world annotations
/// have none: they are never members).
pub fn item_level(doc: &Document, item: &SelItem) -> Option<LevelIx> {
    match item {
        SelItem::Entity(id) => doc.find_entity(id).map(|f| f.0),
        SelItem::Note(id) => crate::annot::find_note(doc, id).and_then(|r| r.level()),
        _ => None,
    }
}

fn item_id(item: &SelItem) -> Option<&Id> {
    match item {
        SelItem::Entity(id) | SelItem::Note(id) => Some(id),
        _ => None,
    }
}

/// The group `item` is a member of, with its level.
pub fn group_of<'d>(doc: &'d Document, item: &SelItem) -> Option<(LevelIx, &'d ObjectGroup)> {
    let ix = item_level(doc, item)?;
    let g = doc.level(ix).group_of(item_id(item)?)?;
    Some((ix, g))
}

/// The selection item of a group member: an entity, else an annotation.
fn member_item(doc: &Document, ix: LevelIx, id: &Id) -> Option<SelItem> {
    let l = doc.level(ix);
    if l.all_entities().any(|e| e.id == *id) {
        Some(SelItem::Entity(id.clone()))
    } else if l.notes.iter().any(|n| n.id == *id) {
        Some(SelItem::Note(id.clone()))
    } else {
        None
    }
}

/// What a Select click on `item` selects: `item` alone when it isn't a
/// member or its group is the entered one, else `item` (first) and every
/// other member of its group that the level still has. A click outside the
/// entered group leaves it.
pub fn click_items(doc: &Document, view: &mut ViewState, item: SelItem) -> Vec<SelItem> {
    let Some((ix, g)) = group_of(doc, &item) else {
        view.entered_group = None;
        return vec![item];
    };
    let key = (doc.ids[ix].clone(), g.id.clone());
    if view.entered_group.as_ref() == Some(&key) {
        return vec![item];
    }
    view.entered_group = None;
    let mut out = vec![item.clone()];
    out.extend(
        g.members
            .iter()
            .filter_map(|m| member_item(doc, ix, m))
            .filter(|m| *m != item),
    );
    out
}

/// A double click with Select on `item`: enter its group (if it is a
/// member), selecting `item` alone. Returns whether it entered one.
pub fn enter(doc: &Document, view: &mut ViewState, item: &SelItem) -> bool {
    let Some((ix, g)) = group_of(doc, item) else {
        return false;
    };
    view.entered_group = Some((doc.ids[ix].clone(), g.id.clone()));
    view.selection.set([item.clone()]);
    true
}

/// The level and ids of the selected entities and level annotations, when
/// they are all in one level.
fn selected_members(doc: &Document, view: &ViewState) -> Result<(LevelIx, Vec<Id>), String> {
    let mut level = None;
    let mut ids = Vec::new();
    for item in &view.selection.items {
        let Some(id) = item_id(item) else {
            continue;
        };
        let Some(ix) = item_level(doc, item) else {
            return Err("World annotations can't be grouped: only a level's".into());
        };
        if level.is_some_and(|l| l != ix) {
            return Err("Group things of one level at a time".into());
        }
        level = Some(ix);
        ids.push(id.clone());
    }
    match level {
        Some(ix) => Ok((ix, ids)),
        None => Err("Select entities or annotations to group".into()),
    }
}

/// Ctrl+G: group the selected entities and annotations (of one level).
pub fn group_command(doc: &Document, view: &ViewState) -> Result<Command, String> {
    let (ix, ids) = selected_members(doc, view)?;
    core_groups::group(&doc.project, &doc.level(ix).slug, &ids, None, None)
        .map_err(|e| e.to_string())
}

/// Ctrl+Shift+G: remove the groups the selection's members are in (their
/// members stay), one entry.
pub fn ungroup_command(doc: &Document, view: &ViewState) -> Result<Command, String> {
    let mut cmds = Vec::new();
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in &view.selection.items {
        if let Some((ix, g)) = group_of(doc, item)
            && seen.insert((ix, g.id.clone()))
        {
            names.push(g.name.clone());
            cmds.push(Command::RemoveGroup {
                level: doc.level(ix).slug.clone(),
                id: g.id.clone(),
            });
        }
    }
    if cmds.is_empty() {
        return Err("Nothing selected is in a group".into());
    }
    let label = match names.as_slice() {
        [one] => format!("Ungroup {one}"),
        many => format!("Ungroup {} groups", many.len()),
    };
    Ok(Command::Batch {
        label,
        commands: cmds,
    })
}

/// The Inspector's group part: for the selection's group(s), the name
/// (editable) and member count with Ungroup; for an ungrouped selection of
/// one level, Group. Returns how many rows it showed.
pub fn inspector_section(ui: &mut egui::Ui, st: &mut EditorState) -> usize {
    let EditorState {
        doc,
        view,
        pending,
        edit_bufs,
        keymap,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        return 0;
    };
    let mut groups: Vec<(LevelIx, &ObjectGroup)> = Vec::new();
    let mut loose = 0;
    for item in &view.selection.items {
        match group_of(doc, item) {
            Some((ix, g)) if !groups.iter().any(|(i, h)| *i == ix && h.id == g.id) => {
                groups.push((ix, g));
            }
            Some(_) => {}
            None if item_id(item).is_some() => loose += 1,
            None => {}
        }
    }
    if groups.is_empty() && loose < 2 {
        return 0;
    }
    let mut ungroup = false;
    let mut group = false;
    for (ix, g) in &groups {
        let entered = view.entered_group.as_ref() == Some(&(doc.ids[*ix].clone(), g.id.clone()));
        ui.horizontal(|ui| {
            ui.label("Group");
            let key = format!("group:{}:name", g.id);
            let buf = edit_bufs
                .entry(key.clone())
                .or_insert_with(|| g.name.clone());
            let r = ui.add(egui::TextEdit::singleline(buf).desired_width(140.0));
            mark(ui, &key, r.rect);
            if r.lost_focus() {
                let name = buf.trim().to_string();
                edit_bufs.remove(&key);
                if !name.is_empty() && name != g.name {
                    pending.push(Command::SetGroup {
                        level: doc.level(*ix).slug.clone(),
                        group: ObjectGroup {
                            name,
                            ..(*g).clone()
                        },
                        index: None,
                    });
                }
            } else if !r.has_focus() && *buf != g.name {
                *buf = g.name.clone();
            }
            ui.weak(format!(
                "{} member{}{}",
                g.members.len(),
                if g.members.len() == 1 { "" } else { "s" },
                if entered { ", entered" } else { "" }
            ));
        });
    }
    ui.horizontal(|ui| {
        if !groups.is_empty() {
            let r = ui.small_button("Ungroup").on_hover_text(keymap.hint(
                "Ungroup: the members stay, the group goes",
                crate::keymap::Cmd::Ungroup,
            ));
            mark(ui, "group:ungroup", r.rect);
            ungroup = r.clicked();
        }
        if loose > 0 || groups.len() > 1 {
            let r = ui.small_button("Group").on_hover_text(keymap.hint(
                "Group: one group of everything selected",
                crate::keymap::Cmd::Group,
            ));
            mark(ui, "group:group", r.rect);
            group = r.clicked();
        }
        if !groups.is_empty() {
            ui.weak("double click a member to pick it alone");
        }
    });
    let cmd = if ungroup {
        Some(ungroup_command(doc, view))
    } else if group {
        Some(group_command(doc, view))
    } else {
        None
    };
    match cmd {
        Some(Ok(c)) => pending.push(c),
        Some(Err(e)) => view.set_status(e, true),
        None => {}
    }
    groups.len().max(1)
}

#[cfg(test)]
mod tests {
    use super::super::testkit::state;
    use super::*;

    fn id(x: &str) -> Id {
        Id::new(x).unwrap()
    }

    #[test]
    fn a_click_takes_the_group_a_double_click_one_member() {
        let (_tmp, mut st) = state();
        let doc = st.doc.as_mut().unwrap();
        let c = core_groups::group(
            &doc.project,
            &worlds_core::slug("crevasse"),
            &[id("btncrev222"), id("notecrev22")],
            Some("Gap".into()),
            Some(id("gapgroup22")),
        )
        .unwrap();
        doc.project.apply(c).unwrap();
        let doc = st.doc.as_ref().unwrap();
        let btn = SelItem::Entity(id("btncrev222"));
        assert_eq!(
            click_items(doc, &mut st.view, btn.clone()),
            [btn.clone(), SelItem::Note(id("notecrev22"))]
        );
        // Not a member: alone.
        let door = SelItem::Entity(id("doorcrev22"));
        assert_eq!(
            click_items(doc, &mut st.view, door.clone()),
            vec![door.clone()]
        );
        // Entered: one member at a time, until a click elsewhere.
        assert!(enter(doc, &mut st.view, &btn));
        assert_eq!(st.view.selection.items, vec![btn.clone()]);
        let note = SelItem::Note(id("notecrev22"));
        assert_eq!(
            click_items(doc, &mut st.view, note.clone()),
            vec![note.clone()]
        );
        click_items(doc, &mut st.view, door);
        assert_eq!(st.view.entered_group, None);
        assert_eq!(click_items(doc, &mut st.view, note).len(), 2);
        // Ungroup: one entry naming it.
        st.view.selection.set([btn]);
        let c = ungroup_command(doc, &st.view).unwrap();
        assert_eq!(c.label(), "Ungroup Gap");
    }

    #[test]
    fn group_takes_one_level() {
        let (_tmp, mut st) = state();
        let doc = st.doc.as_ref().unwrap();
        st.view.selection.set([
            SelItem::Entity(id("btncrev222")),
            SelItem::Entity(id("doorcrev22")),
        ]);
        let c = group_command(doc, &st.view).unwrap();
        assert_eq!(c.label(), "Group 2 objects");
        st.view.selection.set([
            SelItem::Entity(id("btncrev222")),
            SelItem::Entity(id("doorshaft2")),
        ]);
        assert!(group_command(doc, &st.view).is_err());
        st.view.selection.clear();
        assert!(group_command(doc, &st.view).is_err());
    }
}
