use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

use super::RawData;
use crate::types::*;

#[derive(Clone, Copy)]
pub(crate) struct ObjectPresenceCandidate {
    pub(crate) id: u32,
    pub(crate) index: usize,
    pub(crate) size: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct ObjectGrowthCandidate {
    pub(crate) id: u32,
    pub(crate) baseline_index: usize,
    pub(crate) profile_index: usize,
    pub(crate) baseline_size: usize,
    pub(crate) profile_size: usize,
    pub(crate) delta: i64,
}

/// Generic bounded top-K eviction helper.
pub(crate) fn push_bounded_by_key<T, K: Ord>(
    candidates: &mut Vec<T>,
    item: T,
    limit: usize,
    key: impl Fn(&T) -> K,
) {
    if limit == 0 {
        return;
    }
    candidates.push(item);
    if candidates.len() > limit {
        let worst = (0..candidates.len())
            .min_by_key(|&i| key(&candidates[i]))
            .unwrap();
        candidates.swap_remove(worst);
    }
}

pub(crate) fn sorted_node_indices(ids: &[u32]) -> Option<Vec<u32>> {
    if ids.windows(2).all(|w| w[0] <= w[1]) {
        return None;
    }
    let mut order: Vec<u32> = (0..ids.len() as u32).collect();
    order.par_sort_unstable_by_key(|&index| ids[index as usize]);
    Some(order)
}

#[inline]
pub(crate) fn ordered_id_index(
    ids: &[u32],
    order: Option<&Vec<u32>>,
    position: usize,
) -> (u32, usize) {
    let index = order
        .and_then(|items| items.get(position).copied())
        .map(|index| index as usize)
        .unwrap_or(position);
    (ids[index], index)
}

pub(crate) fn snapshot_object(raw: &RawData, index: usize) -> SnapshotObject {
    let node = super::HeapSnapshot::create_node(raw, index);
    SnapshotObject {
        index,
        id: node.id,
        name: node.name,
        type_: node.type_,
        self_size: node.self_size,
        edge_count: node.edge_count,
    }
}

pub(crate) fn diff_snapshot_objects(
    baseline: &RawData,
    profile: &RawData,
    limit: usize,
) -> SnapshotObjectDiff {
    let baseline_ids = &baseline.nodes.ids;
    let profile_ids = &profile.nodes.ids;
    let baseline_order = sorted_node_indices(baseline_ids);
    let profile_order = sorted_node_indices(profile_ids);

    let mut base_pos = 0usize;
    let mut profile_pos = 0usize;
    let mut matched_count = 0usize;
    let mut new_count = 0usize;
    let mut deleted_count = 0usize;
    let mut new_size = 0usize;
    let mut deleted_size = 0usize;
    let mut delta_size = 0i64;
    let mut new_objects = Vec::with_capacity(limit.min(64));
    let mut deleted_objects = Vec::with_capacity(limit.min(64));
    let mut grown_objects = Vec::with_capacity(limit.min(64));

    while base_pos < baseline_ids.len() || profile_pos < profile_ids.len() {
        let base = (base_pos < baseline_ids.len())
            .then(|| ordered_id_index(baseline_ids, baseline_order.as_ref(), base_pos));
        let current = (profile_pos < profile_ids.len())
            .then(|| ordered_id_index(profile_ids, profile_order.as_ref(), profile_pos));

        match (base, current) {
            (Some((base_id, base_index)), Some((profile_id, profile_index)))
                if base_id == profile_id =>
            {
                matched_count += 1;
                let baseline_node_size = baseline.nodes.self_sizes[base_index] as usize;
                let profile_node_size = profile.nodes.self_sizes[profile_index] as usize;
                let delta = profile_node_size as i64 - baseline_node_size as i64;
                delta_size += delta;
                if delta > 0 {
                    push_bounded_by_key(
                        &mut grown_objects,
                        ObjectGrowthCandidate {
                            id: profile_id,
                            baseline_index: base_index,
                            profile_index,
                            baseline_size: baseline_node_size,
                            profile_size: profile_node_size,
                            delta,
                        },
                        limit,
                        |item| (item.delta, item.id),
                    );
                }
                base_pos += 1;
                profile_pos += 1;
            }
            (Some((base_id, base_index)), Some((profile_id, _))) if base_id < profile_id => {
                let size = baseline.nodes.self_sizes[base_index] as usize;
                deleted_count += 1;
                deleted_size += size;
                delta_size -= size as i64;
                push_bounded_by_key(
                    &mut deleted_objects,
                    ObjectPresenceCandidate {
                        id: base_id,
                        index: base_index,
                        size,
                    },
                    limit,
                    |item| (item.size, item.id),
                );
                base_pos += 1;
            }
            (Some((_base_id, _)), Some((_profile_id, profile_index))) => {
                let size = profile.nodes.self_sizes[profile_index] as usize;
                new_count += 1;
                new_size += size;
                delta_size += size as i64;
                push_bounded_by_key(
                    &mut new_objects,
                    ObjectPresenceCandidate {
                        id: profile_ids[profile_index],
                        index: profile_index,
                        size,
                    },
                    limit,
                    |item| (item.size, item.id),
                );
                profile_pos += 1;
            }
            (Some((base_id, base_index)), None) => {
                let size = baseline.nodes.self_sizes[base_index] as usize;
                deleted_count += 1;
                deleted_size += size;
                delta_size -= size as i64;
                push_bounded_by_key(
                    &mut deleted_objects,
                    ObjectPresenceCandidate {
                        id: base_id,
                        index: base_index,
                        size,
                    },
                    limit,
                    |item| (item.size, item.id),
                );
                base_pos += 1;
            }
            (None, Some((_profile_id, profile_index))) => {
                let size = profile.nodes.self_sizes[profile_index] as usize;
                new_count += 1;
                new_size += size;
                delta_size += size as i64;
                push_bounded_by_key(
                    &mut new_objects,
                    ObjectPresenceCandidate {
                        id: profile_ids[profile_index],
                        index: profile_index,
                        size,
                    },
                    limit,
                    |item| (item.size, item.id),
                );
                profile_pos += 1;
            }
            (None, None) => break,
        }
    }

    new_objects.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.id.cmp(&b.id)));
    deleted_objects.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.id.cmp(&b.id)));
    grown_objects.sort_unstable_by(|a, b| b.delta.cmp(&a.delta).then_with(|| a.id.cmp(&b.id)));

    SnapshotObjectDiff {
        matched_count,
        new_count,
        deleted_count,
        new_size,
        deleted_size,
        delta_size,
        new_objects: new_objects
            .into_iter()
            .map(|candidate| snapshot_object(profile, candidate.index))
            .collect(),
        deleted_objects: deleted_objects
            .into_iter()
            .map(|candidate| snapshot_object(baseline, candidate.index))
            .collect(),
        grown_objects: grown_objects
            .into_iter()
            .map(|candidate| {
                let node = snapshot_object(profile, candidate.profile_index);
                SnapshotObjectChange {
                    id: candidate.id as usize,
                    baseline_index: candidate.baseline_index,
                    profile_index: candidate.profile_index,
                    name: node.name,
                    type_: node.type_,
                    baseline_size: candidate.baseline_size,
                    profile_size: candidate.profile_size,
                    delta: candidate.delta,
                }
            })
            .collect(),
    }
}

pub(crate) fn diff_snapshot_maps(
    baseline: &HashMap<String, TypeSummary>,
    profile: &HashMap<String, TypeSummary>,
) -> Vec<DiffEntry> {
    let mut keys: HashSet<String> = HashSet::new();
    keys.extend(baseline.keys().cloned());
    keys.extend(profile.keys().cloned());

    let mut entries: Vec<DiffEntry> = keys
        .into_iter()
        .map(|k| {
            let b = baseline.get(&k).map(|e| e.size).unwrap_or(0) as i64;
            let p = profile.get(&k).map(|e| e.size).unwrap_or(0) as i64;
            let delta = p - b;
            let delta_pct = if b == 0 {
                None
            } else {
                Some(delta as f64 / b as f64)
            };
            DiffEntry {
                name: k,
                baseline_size: b as usize,
                profile_size: p as usize,
                delta,
                delta_pct,
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        b.delta
            .abs()
            .cmp(&a.delta.abs())
            .then_with(|| a.name.cmp(&b.name))
    });
    entries
}
