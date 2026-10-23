//! The text view (`worlds_core::text::render_level`).

mod common;

use worlds_core::text::{NoteFilter, render_level, render_level_with};

#[test]
fn crevasse_text_view() {
    let p = common::fixture();
    insta::assert_snapshot!("crevasse_text", render_level(&p, "crevasse", None).unwrap());
}

#[test]
fn layer_filter_and_errors() {
    let p = common::fixture();
    let t = render_level(&p, "shaft", Some(&["collision"])).unwrap();
    assert!(t.contains("int_grid collision (6x8 cells of 16 px)"), "{t}");
    assert!(!t.contains("decor_back"), "{t}");
    assert!(t.contains("\nentities: 1\n"), "{t}");
    assert!(render_level(&p, "nowhere", None).is_err());
    assert!(render_level(&p, "shaft", Some(&["nope"])).is_err());
}

#[test]
fn note_filter() {
    let p = common::fixture();
    let all = render_level_with(&p, "crevasse", None, NoteFilter::All).unwrap();
    assert_eq!(all, render_level(&p, "crevasse", None).unwrap());
    assert!(
        all.contains("\nnotes: 2\n") && all.contains("pathcrev22"),
        "{all}"
    );
    let open = render_level_with(&p, "crevasse", None, NoteFilter::Open).unwrap();
    assert!(
        open.contains("\nnotes: 1\n  notecrev22 note open"),
        "{open}"
    );
    assert!(!open.contains("pathcrev22"), "{open}");
    let none = render_level_with(&p, "crevasse", None, NoteFilter::None).unwrap();
    assert!(!none.contains("notes:") && none.ends_with("\n"), "{none}");
}

#[test]
fn level_tags_and_world_notes_over_the_level() {
    use worlds_core::{Annotation, GuideGroup, NoteStatus, Shape, slug};
    let mut p = common::fixture();
    let w = &mut p.worlds[0];
    w.levels
        .iter_mut()
        .find(|l| l.slug == "crevasse")
        .unwrap()
        .tags = vec!["forest".into(), "ruined".into()];
    // A done world note over crevasse, and one far away.
    let mut done = Annotation::new(Shape::Note { px: [10, 10] });
    done.id = worlds_core::Id::new("donenote22").unwrap();
    done.status = NoteStatus::Done;
    w.notes.push(done);
    let mut far = Annotation::new(Shape::Pin {
        px: [5000, 5000],
        icon: None,
        label: None,
    });
    far.id = worlds_core::Id::new("farpin2222").unwrap();
    w.notes.push(far);
    // Guides count as built: this one is designed far below crevasse but
    // its group moves it over it; guidestem2 is moved away.
    w.guide_groups = vec![
        GuideGroup {
            offset: [0, -1000],
            ..GuideGroup::new(slug("up"))
        },
        GuideGroup {
            offset: [5000, 0],
            ..GuideGroup::new(slug("away"))
        },
    ];
    let mut g = Annotation::new(Shape::Guide {
        points: vec![[10, 1010], [20, 1020]],
        closed: false,
        group: Some(slug("up")),
    });
    g.id = worlds_core::Id::new("guideup222").unwrap();
    w.notes.push(g);
    if let Shape::Guide { group, .. } = &mut w.notes[0].shape {
        *group = Some(slug("away"));
    }
    // A guide line shows its group (in level notes too, where validation
    // reports it).
    let mut lg = Annotation::new(Shape::Guide {
        points: vec![[0, 0], [0, 10]],
        closed: true,
        group: Some(slug("up")),
    });
    lg.id = worlds_core::Id::new("levelguide").unwrap();
    lg.locked = true;
    w.levels
        .iter_mut()
        .find(|l| l.slug == "crevasse")
        .unwrap()
        .notes
        .push(lg);
    insta::assert_snapshot!(
        "crevasse_text_tags",
        render_level(&p, "crevasse", None).unwrap()
    );
    let open = render_level_with(&p, "crevasse", None, NoteFilter::Open).unwrap();
    assert!(
        open.contains("\nworld notes over this level: 2 (world pixels)\n"),
        "{open}"
    );
    assert!(!open.contains("donenote22"), "{open}");
    let none = render_level_with(&p, "crevasse", None, NoteFilter::None).unwrap();
    assert!(!none.contains("world notes") && !none.contains("guides over"));
    // Shaft has no tags: its header is unchanged.
    let shaft = render_level(&p, "shaft", None).unwrap();
    assert!(
        shaft.starts_with("level shaft \"Shaft\" world main id shaft22222\n"),
        "{shaft}"
    );
}
