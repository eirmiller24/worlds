mod common;

use worlds_core::slug;
use worlds_rules::{Error, TilesetImages};

/// 3 x 1 tiles of 16 px, but the image is 40 px wide: the third column is a
/// partial 8 px tile.
fn write_png(dir: &std::path::Path) {
    let mut img = image::RgbaImage::from_pixel(40, 16, image::Rgba([10, 20, 30, 255]));
    img.put_pixel(20, 5, image::Rgba([0, 0, 0, 254])); // tile [1, 0] not opaque
    img.put_pixel(39, 0, image::Rgba([0, 0, 0, 0])); // tile [2, 0] runs past the edge anyway
    img.save(dir.join("main.png")).unwrap();
}

#[test]
fn opacity_from_png() {
    let dir = tempfile::tempdir().unwrap();
    write_png(dir.path());
    let p = common::project();
    let imgs = TilesetImages::new(dir.path(), &p.tilesets);
    assert!(imgs.is_opaque("main", 0, 0).unwrap());
    assert!(!imgs.is_opaque("main", 1, 0).unwrap());
    assert!(
        imgs.is_opaque("main", 2, 0).unwrap(),
        "rect past the image edge counts as opaque"
    );
    assert!(
        !imgs.is_opaque("main", 3, 0).unwrap(),
        "entirely outside the image"
    );
    assert_eq!(imgs.opacity("main").unwrap().grid(), [3, 1]);
}

#[test]
fn missing_image_names_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let p = common::project();
    let imgs = TilesetImages::new(dir.path(), &p.tilesets);
    let e = imgs.is_opaque("alt", 0, 0).unwrap_err();
    assert!(matches!(e, Error::Image { .. }));
    assert!(e.to_string().contains("alt.png"), "{e}");
    assert!(matches!(
        imgs.opacity("nope"),
        Err(Error::UnknownTileset(_))
    ));
}

#[test]
fn images_load_lazily() {
    // Rules whose tiles can't lock implicitly (alpha < 1) never need the
    // image, so a project with no PNGs on disk still bakes.
    let dir = tempfile::tempdir().unwrap();
    let mut p = common::project();
    let mut r = common::rule("r", &["+"], vec![worlds_core::TileAlt::single(0, 0)]);
    r.out_mut().unwrap().draw_mut().alpha = 0.5;
    common::set_groups(&mut p, vec![common::group("g", vec![r.clone()])]);
    let l = common::level(&p, &["#"]);
    let imgs = TilesetImages::new(dir.path(), &p.tilesets);
    assert!(worlds_rules::bake_layer(&p, &l, &slug("auto"), &imgs).is_ok());
    // An eligible rule does need it, and the error surfaces.
    r.out_mut().unwrap().draw_mut().alpha = 1.0;
    common::set_groups(&mut p, vec![common::group("g", vec![r])]);
    assert!(worlds_rules::bake_layer(&p, &l, &slug("auto"), &imgs).is_err());
    write_png(dir.path());
    assert!(worlds_rules::bake_layer(&p, &l, &slug("auto"), &imgs).is_ok());
}
