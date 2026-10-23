//! Per-connection request queues and the order the UI thread drains them
//! in (`docs/api.md`, "Queue and ordering").
//!
//! Each connection's requests run one at a time, in the order sent. Across
//! connections the oldest ready request goes first, except that while one
//! connection has a batch open, another connection's write stays at the
//! head of its queue (holding up that connection only).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Instant;

use serde_json::Value;

use crate::handler::{ConnId, is_write};

/// One queued request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Queued {
    /// Arrival order across all connections.
    pub seq: u64,
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Debug)]
struct Lane {
    items: VecDeque<Queued>,
    /// A request of this connection is being handled or waits for approval.
    busy: bool,
    last_activity: Instant,
}

/// Every connection's queue.
#[derive(Debug, Default)]
pub(crate) struct Queues {
    next_seq: u64,
    lanes: BTreeMap<ConnId, Lane>,
}

impl Queues {
    pub fn add_conn(&mut self, conn: ConnId) {
        self.lanes.insert(
            conn,
            Lane {
                items: VecDeque::new(),
                busy: false,
                last_activity: Instant::now(),
            },
        );
    }

    /// Forget a connection and whatever it still had queued.
    pub fn remove_conn(&mut self, conn: ConnId) {
        self.lanes.remove(&conn);
    }

    /// Queue a request. False if the connection is gone.
    pub fn push(&mut self, conn: ConnId, id: Value, method: String, params: Value) -> bool {
        let Some(lane) = self.lanes.get_mut(&conn) else {
            return false;
        };
        self.next_seq += 1;
        lane.items.push_back(Queued {
            seq: self.next_seq,
            id,
            method,
            params,
        });
        lane.last_activity = Instant::now();
        true
    }

    /// The next request to handle, marking its connection busy. `open`
    /// holds the connections with an open batch.
    pub fn pop_next(&mut self, open: &BTreeSet<ConnId>) -> Option<(ConnId, Queued)> {
        let conn = self
            .lanes
            .iter()
            .filter(|(_, l)| !l.busy)
            .filter_map(|(c, l)| l.items.front().map(|q| (*c, q)))
            .filter(|(c, q)| !(is_write(&q.method) && open.iter().any(|o| o != c)))
            .min_by_key(|(_, q)| q.seq)
            .map(|(c, _)| c)?;
        let lane = self.lanes.get_mut(&conn)?;
        let q = lane.items.pop_front()?;
        lane.busy = true;
        Some((conn, q))
    }

    /// The connection's request is answered (or no longer blocks it).
    pub fn set_idle(&mut self, conn: ConnId) {
        if let Some(l) = self.lanes.get_mut(&conn) {
            l.busy = false;
        }
    }

    /// When the connection last queued a request.
    pub fn last_activity(&self, conn: ConnId) -> Option<Instant> {
        self.lanes.get(&conn).map(|l| l.last_activity)
    }

    /// Whether anything is queued at all.
    pub fn is_empty(&self) -> bool {
        self.lanes.values().all(|l| l.items.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn push(q: &mut Queues, conn: ConnId, method: &str) {
        assert!(q.push(conn, json!(method), method.into(), json!({})));
    }

    fn next(q: &mut Queues, open: &[ConnId]) -> Option<(ConnId, String)> {
        let open: BTreeSet<ConnId> = open.iter().copied().collect();
        q.pop_next(&open).map(|(c, item)| {
            q.set_idle(c);
            (c, item.method)
        })
    }

    #[test]
    fn arrival_order_across_connections() {
        let mut q = Queues::default();
        q.add_conn(1);
        q.add_conn(2);
        push(&mut q, 2, "level.list");
        push(&mut q, 1, "grid.fill_rect");
        push(&mut q, 2, "grid.line");
        assert_eq!(next(&mut q, &[]), Some((2, "level.list".into())));
        assert_eq!(next(&mut q, &[]), Some((1, "grid.fill_rect".into())));
        assert_eq!(next(&mut q, &[]), Some((2, "grid.line".into())));
        assert_eq!(next(&mut q, &[]), None);
    }

    #[test]
    fn writes_park_behind_another_connections_batch() {
        let mut q = Queues::default();
        for c in 1..=3 {
            q.add_conn(c);
        }
        // 1 has a batch open. 2's write parks and holds up 2's read; 3 goes.
        push(&mut q, 2, "grid.fill_rect");
        push(&mut q, 2, "level.list");
        push(&mut q, 3, "entity.list");
        push(&mut q, 1, "grid.line");
        assert_eq!(next(&mut q, &[1]), Some((3, "entity.list".into())));
        assert_eq!(next(&mut q, &[1]), Some((1, "grid.line".into())));
        assert_eq!(next(&mut q, &[1]), None);
        // The batch ends: 2 goes on, in order.
        assert_eq!(next(&mut q, &[]), Some((2, "grid.fill_rect".into())));
        assert_eq!(next(&mut q, &[]), Some((2, "level.list".into())));
    }

    #[test]
    fn a_busy_connection_waits() {
        let mut q = Queues::default();
        q.add_conn(1);
        push(&mut q, 1, "grid.fill_rect");
        push(&mut q, 1, "level.list");
        let open = BTreeSet::new();
        assert!(q.pop_next(&open).is_some());
        // Waiting for approval: its next request stays queued.
        assert!(q.pop_next(&open).is_none());
        q.set_idle(1);
        assert_eq!(
            q.pop_next(&open).map(|(_, i)| i.method),
            Some("level.list".into())
        );
        q.remove_conn(1);
        assert!(!q.push(1, json!(1), "x".into(), json!({})));
        assert!(q.is_empty());
    }
}
