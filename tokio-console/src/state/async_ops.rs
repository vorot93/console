use crate::{
    intern::{self, InternedStr},
    state::{
        pb_duration,
        resources::Resource,
        store::{self, Id, Store},
        tasks::Task,
        Attribute, Field, Metadata, Visibility,
    },
    view,
};
use console_api as proto;
use ratatui::text::Span;
use std::{
    cell::RefCell,
    collections::HashMap,
    convert::{TryFrom, TryInto},
    rc::{Rc, Weak},
    time::{Duration, SystemTime},
};

#[derive(Default, Debug)]
pub(crate) struct AsyncOpsState {
    async_ops: Store<AsyncOp>,
    dropped_events: u64,
}

#[derive(Debug, Copy, Clone)]
#[repr(usize)]
pub(crate) enum SortBy {
    Aid = 0,
    Task = 1,
    Source = 2,
    Total = 3,
    Busy = 4,
    Idle = 5,
    Polls = 6,
}

#[derive(Debug)]
pub(crate) struct AsyncOp {
    id: Id<AsyncOp>,
    parent_id: InternedStr,
    resource_id: Id<Resource>,
    meta_id: u64,
    source: InternedStr,
    stats: AsyncOpStats,
}

pub(crate) type AsyncOpRef = store::Ref<AsyncOp>;

#[derive(Debug)]
struct AsyncOpStats {
    created_at: SystemTime,
    dropped_at: Option<SystemTime>,

    polls: u64,
    busy: Duration,
    last_poll_started: Option<SystemTime>,
    last_poll_ended: Option<SystemTime>,
    idle: Option<Duration>,
    total: Option<Duration>,
    task_id: Option<Id<Task>>,
    task_id_str: InternedStr,
    formatted_attributes: Vec<Vec<Span<'static>>>,
}

impl Default for SortBy {
    fn default() -> Self {
        Self::Aid
    }
}

impl SortBy {
    pub fn sort(&self, now: SystemTime, ops: &mut [Weak<RefCell<AsyncOp>>]) {
        match self {
            Self::Aid => ops.sort_unstable_by_key(|ao| ao.upgrade().map(|a| a.borrow().id)),
            Self::Task => ops.sort_unstable_by_key(|ao| ao.upgrade().map(|a| a.borrow().task_id())),
            Self::Source => {
                ops.sort_unstable_by_key(|ao| ao.upgrade().map(|a| a.borrow().source.clone()))
            }
            Self::Total => {
                ops.sort_unstable_by_key(|ao| ao.upgrade().map(|a| a.borrow().total(now)))
            }
            Self::Busy => ops.sort_unstable_by_key(|ao| ao.upgrade().map(|a| a.borrow().busy(now))),
            Self::Idle => ops.sort_unstable_by_key(|ao| ao.upgrade().map(|a| a.borrow().idle(now))),
            Self::Polls => {
                ops.sort_unstable_by_key(|ao| ao.upgrade().map(|a| a.borrow().stats.polls))
            }
        }
    }
}

impl TryFrom<usize> for SortBy {
    type Error = ();
    fn try_from(idx: usize) -> Result<Self, Self::Error> {
        match idx {
            idx if idx == Self::Aid as usize => Ok(Self::Aid),
            idx if idx == Self::Task as usize => Ok(Self::Task),
            idx if idx == Self::Source as usize => Ok(Self::Source),
            idx if idx == Self::Total as usize => Ok(Self::Total),
            idx if idx == Self::Busy as usize => Ok(Self::Busy),
            idx if idx == Self::Idle as usize => Ok(Self::Idle),
            idx if idx == Self::Polls as usize => Ok(Self::Polls),
            _ => Err(()),
        }
    }
}

impl view::SortBy for SortBy {
    fn as_column(&self) -> usize {
        *self as usize
    }
}

impl AsyncOpsState {
    /// Returns any new async ops for a resource that were added since the last async ops update.
    pub(crate) fn take_new_async_ops(&mut self) -> impl Iterator<Item = AsyncOpRef> + '_ {
        self.async_ops.take_new_items()
    }

    /// Returns all async ops.
    pub(crate) fn async_ops(&self) -> impl Iterator<Item = AsyncOpRef> + '_ {
        self.async_ops.values().map(Rc::downgrade)
    }

    // Clippy warns us that having too many arguments is bad style. In this case, however
    // it does not make much sense to group any of them.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update_async_ops(
        &mut self,
        styles: &view::Styles,
        strings: &mut intern::Strings,
        metas: &HashMap<u64, Metadata>,
        update: proto::async_ops::AsyncOpUpdate,
        resource_ids: &mut store::Ids<Resource>,
        task_ids: &mut store::Ids<Task>,
        visibility: Visibility,
    ) {
        let mut stats_update = update.stats_update;

        self.async_ops
            .insert_with(visibility, update.new_async_ops, |ids, async_op| {
                let span_id = match async_op.id.as_ref() {
                    Some(id) => id.id,
                    None => {
                        tracing::warn!(?async_op, "skipping async op with no id");
                        return None;
                    }
                };
                let meta_id = match async_op.metadata.as_ref() {
                    Some(id) => id.id,
                    None => {
                        tracing::warn!(?async_op, "async op has no metadata id, skipping");
                        return None;
                    }
                };
                let meta = match metas.get(&meta_id) {
                    Some(meta) => meta,
                    None => {
                        tracing::warn!(?async_op, meta_id, "no metadata for async op, skipping");
                        return None;
                    }
                };

                let stats = AsyncOpStats::from_proto(
                    stats_update.remove(&span_id)?,
                    meta,
                    styles,
                    strings,
                    task_ids,
                );

                let id = ids.id_for(span_id);
                let resource_id = resource_ids.id_for(async_op.resource_id?.id);
                let parent_id = match async_op.parent_async_op_id {
                    Some(id) => strings.string(format!("{}", ids.id_for(id.id))),
                    None => strings.string("n/a".to_string()),
                };

                let source = strings.string(async_op.source);

                let async_op = AsyncOp {
                    id,
                    parent_id,
                    resource_id,
                    meta_id,
                    source,
                    stats,
                };
                Some((id, async_op))
            });

        for (stats, mut async_op) in self.async_ops.updated(stats_update) {
            if let Some(meta) = metas.get(&async_op.meta_id) {
                tracing::trace!(?async_op, ?stats, "processing stats update for");
                async_op.stats = AsyncOpStats::from_proto(stats, meta, styles, strings, task_ids);
            }
        }

        self.dropped_events += update.dropped_events;
    }

    pub(crate) fn retain_active(&mut self, now: SystemTime, retain_for: Duration) {
        self.async_ops.retain(|_, async_op| {
            let async_op = async_op.borrow();

            async_op
                .stats
                .dropped_at
                .map(|d| {
                    let dropped_for = now.duration_since(d).unwrap_or_default();
                    retain_for > dropped_for
                })
                .unwrap_or(true)
        })
    }

    pub(crate) fn dropped_events(&self) -> u64 {
        self.dropped_events
    }
}

impl AsyncOp {
    pub(crate) fn id(&self) -> Id<AsyncOp> {
        self.id
    }

    pub(crate) fn parent_id(&self) -> &str {
        &self.parent_id
    }

    pub(crate) fn resource_id(&self) -> Id<Resource> {
        self.resource_id
    }

    pub(crate) fn task_id(&self) -> Option<Id<Task>> {
        self.stats.task_id
    }

    pub(crate) fn task_id_str(&self) -> &str {
        &self.stats.task_id_str
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    pub(crate) fn total(&self, since: SystemTime) -> Duration {
        self.stats
            .total
            .or_else(|| since.duration_since(self.stats.created_at).ok())
            .unwrap_or_default()
    }

    pub(crate) fn busy(&self, since: SystemTime) -> Duration {
        if let (Some(last_poll_started), None) =
            (self.stats.last_poll_started, self.stats.last_poll_ended)
        {
            let current_time_in_poll = since.duration_since(last_poll_started).unwrap_or_default();
            return self.stats.busy + current_time_in_poll;
        }
        self.stats.busy
    }

    pub(crate) fn idle(&self, since: SystemTime) -> Duration {
        self.stats
            .idle
            .or_else(|| self.total(since).checked_sub(self.busy(since)))
            .unwrap_or_default()
    }

    pub(crate) fn total_polls(&self) -> u64 {
        self.stats.polls
    }

    pub(crate) fn dropped(&self) -> bool {
        self.stats.total.is_some()
    }

    pub(crate) fn formatted_attributes(&self) -> &[Vec<Span<'static>>] {
        &self.stats.formatted_attributes
    }
}

impl AsyncOpStats {
    fn from_proto(
        pb: proto::async_ops::Stats,
        meta: &Metadata,
        styles: &view::Styles,
        strings: &mut intern::Strings,
        task_ids: &mut store::Ids<Task>,
    ) -> Self {
        let mut pb = pb;

        let mut attributes = pb
            .attributes
            .drain(..)
            .filter_map(|pb| {
                let field = pb.field?;
                let field = Field::from_proto(field, meta, strings)?;
                Some(Attribute {
                    field,
                    unit: pb.unit,
                })
            })
            .collect::<Vec<_>>();

        let created_at = pb
            .created_at
            .expect("async op span was never created")
            .try_into()
            .unwrap();

        let dropped_at: Option<SystemTime> = pb.dropped_at.map(|v| v.try_into().unwrap());
        let total = dropped_at.map(|d| d.duration_since(created_at).unwrap_or_default());

        let poll_stats = pb.poll_stats.expect("task should have poll stats");
        let busy = poll_stats.busy_time.map(pb_duration).unwrap_or_default();
        let idle = total.map(|total| total.checked_sub(busy).unwrap_or_default());
        let formatted_attributes = Attribute::make_formatted(styles, &mut attributes);
        let task_id = pb.task_id.map(|id| task_ids.id_for(id.id));
        let task_id_str = strings.string(
            task_id
                .as_ref()
                .map(Id::<Task>::to_string)
                .unwrap_or_else(|| "n/a".to_string()),
        );
        Self {
            total,
            idle,
            task_id,
            task_id_str,
            busy,
            last_poll_started: poll_stats.last_poll_started.map(|v| v.try_into().unwrap()),
            last_poll_ended: poll_stats.last_poll_ended.map(|v| v.try_into().unwrap()),
            polls: poll_stats.polls,
            created_at,
            dropped_at,
            formatted_attributes,
        }
    }
}
