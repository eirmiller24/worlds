//! Where a connector's ends are. A connector stores what it joins (a level
//! or an entity) and its waypoints, never its end points: those are
//! computed here, so moving a room or an entity moves the line.

use super::{Endpoint, Level, NoteAt, Project, Shape, World};

/// What a connector end is anchored to, in world pixels: a level's rect
/// (the end sits on its edge) or an entity's pivot (the end sits on it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// The level's center, or the entity's pivot.
    pub point: [i64; 2],
    /// A level's `[x, y, w, h]`; `None` for an entity.
    pub rect: Option<[i64; 4]>,
    /// A non-rectangular level's outline ([`Level::outline`], world
    /// pixels); `None` for a rectangle or an entity. The end sits on it
    /// instead of on the rect.
    pub outline: Option<std::sync::Arc<[[i64; 2]]>>,
}

impl Anchor {
    /// The point where a line toward `toward` leaves this anchor: an
    /// entity's pivot; on a level, the point of its edge nearest `toward`
    /// (`toward` itself when it is on the edge or outside, clamped onto the
    /// rect; from inside, straight out to the nearest edge). A
    /// non-rectangular level's edge is its outline.
    pub fn end_toward(&self, toward: [i64; 2]) -> [i64; 2] {
        match (&self.outline, self.rect) {
            (Some(o), _) => nearest_outline_point(o, toward),
            (None, None) => self.point,
            (None, Some(r)) => nearest_edge_point(r, toward),
        }
    }
}

/// The point of a closed rectilinear outline (a level's, corners in order)
/// nearest `p`, the first segment's on a tie. On a rectangle's outline this
/// is [`nearest_edge_point`] (up to which edge wins a tie).
pub fn nearest_outline_point(outline: &[[i64; 2]], p: [i64; 2]) -> [i64; 2] {
    let n = outline.len();
    let mut best = (i64::MAX, p);
    for i in 0..n {
        let (a, b) = (outline[i], outline[(i + 1) % n]);
        // Axis-aligned segments: clamping onto the box is the nearest point.
        let q = [
            p[0].clamp(a[0].min(b[0]), a[0].max(b[0])),
            p[1].clamp(a[1].min(b[1]), a[1].max(b[1])),
        ];
        let d = (q[0] - p[0]).pow(2) + (q[1] - p[1]).pow(2);
        if d < best.0 {
            best = (d, q);
        }
    }
    best.1
}

/// The point of the edge of rect `[x, y, w, h]` nearest `p`.
pub fn nearest_edge_point(r: [i64; 4], p: [i64; 2]) -> [i64; 2] {
    let [x, y, w, h] = r;
    let (x1, y1) = (x + w, y + h);
    let inside = p[0] > x && p[0] < x1 && p[1] > y && p[1] < y1;
    if !inside {
        return [p[0].clamp(x, x1.max(x)), p[1].clamp(y, y1.max(y))];
    }
    // Inside: out through the nearest edge (left, right, top, bottom first
    // on a tie).
    let d = [p[0] - x, x1 - p[0], p[1] - y, y1 - p[1]];
    let k = (0..4).min_by_key(|&k| d[k]).unwrap_or(0);
    match k {
        0 => [x, p[1]],
        1 => [x1, p[1]],
        2 => [p[0], y],
        _ => [p[0], y1],
    }
}

/// A connector's polyline in world pixels: the `from` end, the waypoints
/// (world pixels), the `to` end. Each end on a level is the point of its
/// edge nearest the next point along the line (the first waypoint, or with
/// none the other end's center or pivot); an end on an entity is its pivot.
pub fn connector_polyline(from: Anchor, to: Anchor, waypoints: &[[i64; 2]]) -> Vec<[i64; 2]> {
    let first = waypoints.first().copied().unwrap_or(to.point);
    let last = waypoints.last().copied().unwrap_or(from.point);
    let mut out = Vec::with_capacity(waypoints.len() + 2);
    out.push(from.end_toward(first));
    out.extend_from_slice(waypoints);
    out.push(to.end_toward(last));
    out
}

impl Project {
    /// A level's rect in world pixels, `[x, y, w, h]`.
    pub fn level_rect(&self, l: &Level) -> [i64; 4] {
        let g = self.grid_size as i64;
        [
            l.world_px[0],
            l.world_px[1],
            l.size[0] as i64 * g,
            l.size[1] as i64 * g,
        ]
    }

    /// The world, level and pivot (world pixels, rounded) of the entity
    /// with this id.
    pub fn entity_pivot(&self, id: &crate::Id) -> Option<(&World, &Level, [i64; 2])> {
        for w in &self.worlds {
            for l in &w.levels {
                for layer in &l.entities {
                    let Some(e) = layer.entities.iter().find(|e| e.id == *id) else {
                        continue;
                    };
                    let lg = self
                        .layer(layer.layer.as_str())
                        .map_or(self.grid_size, |d| self.layer_grid(d))
                        as f64;
                    let pivot = self
                        .entity_def(e.ty.as_str())
                        .map_or([0.0, 0.0], |d| d.pivot);
                    let px = |a: usize| {
                        ((e.cell[a] as f64 + pivot[a]) * lg + e.offset_px[a] as f64).round() as i64
                    };
                    return Some((w, l, [l.world_px[0] + px(0), l.world_px[1] + px(1)]));
                }
            }
        }
        None
    }

    /// Where a connector end is (world pixels), with the world and level it
    /// is in; `None` when it names no level or entity (a dangling end).
    pub fn endpoint_anchor(&self, e: &Endpoint) -> Option<(Anchor, &World, &Level)> {
        match e {
            Endpoint::Level(slug) => self.worlds.iter().find_map(|w| {
                let l = w.level(slug.as_str())?;
                let r = self.level_rect(l);
                Some((
                    Anchor {
                        point: [r[0] + r[2] / 2, r[1] + r[3] / 2],
                        rect: Some(r),
                        outline: l.has_void().then(|| l.outline(self.grid_size).into()),
                    },
                    w,
                    l,
                ))
            }),
            Endpoint::Entity(id) => {
                let (w, l, p) = self.entity_pivot(id)?;
                Some((
                    Anchor {
                        point: p,
                        rect: None,
                        outline: None,
                    },
                    w,
                    l,
                ))
            }
        }
    }

    /// The polyline of a connector in world pixels ([`connector_polyline`]),
    /// its waypoints moved from the annotation's own pixels by its level's
    /// position. `None` for another kind or a dangling end.
    pub fn connector_points(&self, at: NoteAt<'_>) -> Option<Vec<[i64; 2]>> {
        let Shape::Connector {
            from,
            to,
            waypoints,
            ..
        } = &at.note.shape
        else {
            return None;
        };
        let (a, ..) = self.endpoint_anchor(from)?;
        let (b, ..) = self.endpoint_anchor(to)?;
        let o = at.offset();
        let wp: Vec<[i64; 2]> = waypoints
            .iter()
            .map(|p| [p[0] + o[0], p[1] + o[1]])
            .collect();
        Some(connector_polyline(a, b, &wp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM_A: [i64; 4] = [0, 0, 100, 100];
    const ROOM_B: [i64; 4] = [200, 0, 100, 100];

    fn level(r: [i64; 4]) -> Anchor {
        Anchor {
            point: [r[0] + r[2] / 2, r[1] + r[3] / 2],
            rect: Some(r),
            outline: None,
        }
    }

    #[test]
    fn side_by_side_rooms_join_across_the_gap() {
        let p = connector_polyline(level(ROOM_A), level(ROOM_B), &[]);
        assert_eq!(p, vec![[100, 50], [200, 50]]);
    }

    #[test]
    fn diagonal_rooms_meet_at_their_corners() {
        let p = connector_polyline(level(ROOM_A), level([300, 300, 100, 100]), &[]);
        assert_eq!(p, vec![[100, 100], [300, 300]]);
    }

    #[test]
    fn ends_aim_at_the_neighbouring_waypoint() {
        // A waypoint above both rooms: each end leaves through the top.
        let p = connector_polyline(level(ROOM_A), level(ROOM_B), &[[150, -50]]);
        assert_eq!(p, vec![[100, 0], [150, -50], [200, 0]]);
        // A waypoint inside the start room: straight out to its nearest edge.
        let p = connector_polyline(level(ROOM_A), level(ROOM_B), &[[90, 40]]);
        assert_eq!(p[0], [100, 40]);
    }

    #[test]
    fn an_entity_end_is_its_pivot() {
        let e = Anchor {
            point: [250, 80],
            rect: None,
            outline: None,
        };
        let p = connector_polyline(level(ROOM_A), e, &[]);
        assert_eq!(p, vec![[100, 80], [250, 80]]);
    }

    #[test]
    fn a_shaped_room_is_left_through_its_outline() {
        // ROOM_A as an L: its top-right 50x50 quarter cut out.
        let l = Anchor {
            outline: Some(vec![[0, 0], [50, 0], [50, 50], [100, 50], [100, 100], [0, 100]].into()),
            ..level(ROOM_A)
        };
        // A waypoint in the notch: the line leaves through the notch's wall,
        // not the box's top edge at [70, 0].
        let p = connector_polyline(l.clone(), level(ROOM_B), &[[70, 15]]);
        assert_eq!(p[0], [50, 15]);
        // From outside, the nearest point of the outline.
        assert_eq!(l.end_toward([150, 20]), [100, 50]);
        // The lower arm still leaves on the right.
        assert_eq!(l.end_toward([150, 80]), [100, 80]);
        // A rect's outline gives what the rect gives.
        let box_ = [[0, 0], [100, 0], [100, 100], [0, 100]];
        for p in [[150, 50], [-20, -20], [90, 40], [40, 90], [30, 20]] {
            assert_eq!(
                nearest_outline_point(&box_, p),
                nearest_edge_point(ROOM_A, p),
                "{p:?}"
            );
        }
    }
}
