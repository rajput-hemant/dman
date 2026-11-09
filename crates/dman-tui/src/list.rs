//! The downloads list model: sections, sorting, fuzzy filter, paging, cursor and
//! selection. Pure data, no rendering.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use dman_proto::{Job, JobId, JobState};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Default number of stopped jobs per page.
pub const STOPPED_PAGE_SIZE: usize = 50;

/// What the list is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortKey {
    /// Queue order for waiting jobs, start order for active, newest first for stopped.
    Queue,
    /// Name, A to Z.
    Name,
    /// Total size, largest first.
    Size,
    /// Progress, most complete first.
    Progress,
    /// Download speed, fastest first.
    Speed,
    /// ETA, soonest first (unknown last).
    Eta,
    /// Date added, newest first.
    Added,
    /// Upload ratio, highest first.
    Ratio,
}

impl SortKey {
    /// Cycle order.
    pub const ALL: [SortKey; 8] = [
        SortKey::Queue,
        SortKey::Name,
        SortKey::Size,
        SortKey::Progress,
        SortKey::Speed,
        SortKey::Eta,
        SortKey::Added,
        SortKey::Ratio,
    ];

    /// Next key in the cycle.
    pub fn next(self) -> SortKey {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// Label for the info bar.
    pub const fn label(self) -> &'static str {
        match self {
            SortKey::Queue => "queue",
            SortKey::Name => "name",
            SortKey::Size => "size",
            SortKey::Progress => "progress",
            SortKey::Speed => "speed",
            SortKey::Eta => "eta",
            SortKey::Added => "added",
            SortKey::Ratio => "ratio",
        }
    }

    fn compare(self, a: &Job, b: &Job) -> Ordering {
        match self {
            SortKey::Queue => queue_order(a, b),
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Size => b.total_bytes.cmp(&a.total_bytes),
            SortKey::Progress => b.progress().total_cmp(&a.progress()),
            SortKey::Speed => b.download_speed.cmp(&a.download_speed),
            SortKey::Eta => match (a.eta_secs(), b.eta_secs()) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            SortKey::Added => b.added_at.cmp(&a.added_at),
            SortKey::Ratio => b.ratio().total_cmp(&a.ratio()),
        }
    }
}

fn queue_order(a: &Job, b: &Job) -> Ordering {
    match (a.queue_position, b.queue_position) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) if a.state.is_stopped() => {
            let at = a.completed_at.unwrap_or(a.added_at);
            let bt = b.completed_at.unwrap_or(b.added_at);
            bt.cmp(&at)
        }
        (None, None) => a.added_at.cmp(&b.added_at),
    }
}

/// One of the three list sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    /// Transferring or seeding.
    Active,
    /// Queued or paused.
    Waiting,
    /// Complete, failed or removed.
    Stopped,
}

impl Section {
    /// Section a job belongs to.
    pub const fn of(state: JobState) -> Section {
        match state {
            JobState::Active | JobState::Seeding => Section::Active,
            JobState::Waiting | JobState::Paused => Section::Waiting,
            JobState::Complete | JobState::Error | JobState::Removed => Section::Stopped,
        }
    }

    /// Section title.
    pub const fn title(self) -> &'static str {
        match self {
            Section::Active => "Active",
            Section::Waiting => "Waiting",
            Section::Stopped => "Stopped",
        }
    }
}

/// A display row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Section header (not selectable).
    Section {
        /// Which section.
        section: Section,
        /// Matching jobs in the section (all pages).
        total: usize,
        /// Current page (0-based; stopped section only).
        page: usize,
        /// Number of pages (1 for non-paged sections).
        pages: usize,
    },
    /// A job.
    Job(JobId),
}

/// The downloads list state.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadsList {
    /// Sort key.
    pub sort: SortKey,
    /// Reverse the natural direction of the sort key.
    pub reverse: bool,
    /// Fuzzy filter on the name; empty = everything.
    pub filter: String,
    /// Display rows (section headers and jobs), recomputed by [`Self::recompute`].
    pub rows: Vec<Row>,
    /// Visible jobs in display order.
    pub order: Vec<JobId>,
    /// Job under the cursor.
    pub cursor: Option<JobId>,
    /// First display row shown.
    pub offset: usize,
    /// Marked jobs.
    pub selected: BTreeSet<JobId>,
    /// Start of a range selection.
    pub anchor: Option<JobId>,
    /// Current page of the stopped section.
    pub stopped_page: usize,
    /// Jobs per stopped page.
    pub page_size: usize,
}

impl Default for DownloadsList {
    fn default() -> Self {
        Self {
            sort: SortKey::Queue,
            reverse: false,
            filter: String::new(),
            rows: Vec::new(),
            order: Vec::new(),
            cursor: None,
            offset: 0,
            selected: BTreeSet::new(),
            anchor: None,
            stopped_page: 0,
            page_size: STOPPED_PAGE_SIZE,
        }
    }
}

impl DownloadsList {
    /// Rebuild rows from `jobs`, keeping the cursor on the same job if possible.
    pub fn recompute(&mut self, jobs: &HashMap<JobId, Job>) {
        let old_pos = self.cursor_pos();
        let matching = self.matching(jobs);
        let mut sections: [Vec<&Job>; 3] = Default::default();
        for job in matching {
            let i = match Section::of(job.state) {
                Section::Active => 0,
                Section::Waiting => 1,
                Section::Stopped => 2,
            };
            sections[i].push(job);
        }
        let (sort, reverse) = (self.sort, self.reverse);
        for list in &mut sections {
            list.sort_by(|a, b| {
                let o = sort.compare(a, b);
                let o = if reverse { o.reverse() } else { o };
                o.then(a.id.cmp(&b.id))
            });
        }
        self.rows.clear();
        self.order.clear();
        let all = [Section::Active, Section::Waiting, Section::Stopped];
        for (section, list) in all.into_iter().zip(&sections) {
            self.push_section(section, list);
        }
        self.selected.retain(|id| jobs.contains_key(id));
        if self.anchor.is_some_and(|a| !jobs.contains_key(&a)) {
            self.anchor = None;
        }
        self.restore_cursor(old_pos);
    }

    fn matching<'a>(&self, jobs: &'a HashMap<JobId, Job>) -> Vec<&'a Job> {
        if self.filter.trim().is_empty() {
            return jobs.values().collect();
        }
        let pattern = Pattern::parse(&self.filter, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();
        jobs.values()
            .filter(|j| {
                pattern
                    .score(Utf32Str::new(&j.name, &mut buf), &mut matcher)
                    .is_some()
            })
            .collect()
    }

    fn push_section(&mut self, section: Section, list: &[&Job]) {
        if list.is_empty() {
            return;
        }
        let (page, pages, slice) = if section == Section::Stopped {
            let size = self.page_size.max(1);
            let pages = list.len().div_ceil(size);
            self.stopped_page = self.stopped_page.min(pages - 1);
            let start = self.stopped_page * size;
            let end = (start + size).min(list.len());
            (self.stopped_page, pages, &list[start..end])
        } else {
            (0, 1, list)
        };
        self.rows.push(Row::Section {
            section,
            total: list.len(),
            page,
            pages,
        });
        for j in slice {
            self.rows.push(Row::Job(j.id));
            self.order.push(j.id);
        }
    }

    fn restore_cursor(&mut self, old_pos: Option<usize>) {
        if self.cursor.is_some_and(|c| self.order.contains(&c)) {
            return;
        }
        self.cursor = match old_pos {
            Some(p) if !self.order.is_empty() => Some(self.order[p.min(self.order.len() - 1)]),
            _ => self.order.first().copied(),
        };
    }

    /// Index of the cursor job in [`Self::order`].
    pub fn cursor_pos(&self) -> Option<usize> {
        let c = self.cursor?;
        self.order.iter().position(|id| *id == c)
    }

    /// Display row of the cursor job.
    pub fn cursor_row(&self) -> Option<usize> {
        let c = self.cursor?;
        self.rows.iter().position(|r| *r == Row::Job(c))
    }

    /// Move the cursor by `delta` jobs (clamped).
    pub fn move_by(&mut self, delta: isize) {
        if self.order.is_empty() {
            return;
        }
        let pos = self.cursor_pos().unwrap_or(0);
        let last = self.order.len() - 1;
        let new = pos.saturating_add_signed(delta).min(last);
        self.cursor = Some(self.order[new]);
    }

    /// Cursor to the first job.
    pub fn to_top(&mut self) {
        self.cursor = self.order.first().copied();
    }

    /// Cursor to the last job.
    pub fn to_bottom(&mut self) {
        self.cursor = self.order.last().copied();
    }

    /// Put the cursor on the job at display row `row`, if it is a job row.
    pub fn click_row(&mut self, row: usize) -> bool {
        if let Some(Row::Job(id)) = self.rows.get(row) {
            self.cursor = Some(*id);
            true
        } else {
            false
        }
    }

    /// Adjust [`Self::offset`] so the cursor is inside a viewport of `height` rows.
    pub fn ensure_visible(&mut self, height: usize) {
        let height = height.max(1);
        if let Some(r) = self.cursor_row() {
            // Keep the section header above the first job of a section visible.
            let top = if r > 0 && matches!(self.rows[r - 1], Row::Section { .. }) {
                r - 1
            } else {
                r
            };
            if top < self.offset {
                self.offset = top;
            } else if r >= self.offset + height {
                self.offset = r + 1 - height;
            }
        }
        self.offset = self.offset.min(self.rows.len().saturating_sub(height));
    }

    /// Jobs an action applies to: the selection, else the cursor job.
    pub fn targets(&self) -> Vec<JobId> {
        if self.selected.is_empty() {
            self.cursor.into_iter().collect()
        } else {
            self.selected.iter().copied().collect()
        }
    }

    /// Toggle the cursor job's mark and make it the range anchor.
    pub fn toggle_cursor(&mut self) {
        if let Some(c) = self.cursor {
            if !self.selected.remove(&c) {
                self.selected.insert(c);
            }
            self.anchor = Some(c);
        }
    }

    /// Mark every visible job.
    pub fn select_all(&mut self) {
        self.selected.extend(self.order.iter().copied());
    }

    /// Clear marks and anchor.
    pub fn select_none(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }

    /// Mark every job between the anchor and the cursor (inclusive). Without an
    /// anchor, the cursor job becomes the anchor and is marked.
    pub fn select_range(&mut self) {
        let Some(cur) = self.cursor_pos() else { return };
        let anchor = self
            .anchor
            .and_then(|a| self.order.iter().position(|id| *id == a));
        match anchor {
            Some(a) => {
                let (lo, hi) = if a <= cur { (a, cur) } else { (cur, a) };
                self.selected.extend(self.order[lo..=hi].iter().copied());
            }
            None => {
                self.anchor = self.cursor;
                self.selected.extend(self.cursor);
            }
        }
    }

    /// Number of stopped pages for the current rows.
    pub fn stopped_pages(&self) -> usize {
        self.rows
            .iter()
            .find_map(|r| match r {
                Row::Section {
                    section: Section::Stopped,
                    pages,
                    ..
                } => Some(*pages),
                _ => None,
            })
            .unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{job, jobs_map};

    fn list(jobs: &HashMap<JobId, Job>) -> DownloadsList {
        let mut l = DownloadsList::default();
        l.recompute(jobs);
        l
    }

    #[test]
    fn sections_in_order() {
        let jobs = jobs_map(vec![
            job(1, "done.iso", JobState::Complete),
            job(2, "running", JobState::Active),
            job(3, "queued", JobState::Waiting),
        ]);
        let l = list(&jobs);
        assert_eq!(l.order, vec![JobId(2), JobId(3), JobId(1)]);
        assert!(matches!(
            l.rows[0],
            Row::Section {
                section: Section::Active,
                total: 1,
                ..
            }
        ));
        assert_eq!(l.rows.len(), 6);
        assert_eq!(l.cursor, Some(JobId(2)));
    }

    #[test]
    fn sorting_and_reverse() {
        let mut a = job(1, "b", JobState::Active);
        a.download_speed = 10;
        let mut b = job(2, "a", JobState::Active);
        b.download_speed = 20;
        let jobs = jobs_map(vec![a, b]);
        let mut l = list(&jobs);
        l.sort = SortKey::Name;
        l.recompute(&jobs);
        assert_eq!(l.order, vec![JobId(2), JobId(1)]);
        l.sort = SortKey::Speed;
        l.recompute(&jobs);
        assert_eq!(l.order, vec![JobId(2), JobId(1)]);
        l.reverse = true;
        l.recompute(&jobs);
        assert_eq!(l.order, vec![JobId(1), JobId(2)]);
    }

    #[test]
    fn waiting_follows_queue_position() {
        let mut a = job(1, "a", JobState::Waiting);
        a.queue_position = Some(1);
        let mut b = job(2, "b", JobState::Paused);
        b.queue_position = Some(0);
        let l = list(&jobs_map(vec![a, b]));
        assert_eq!(l.order, vec![JobId(2), JobId(1)]);
    }

    #[test]
    fn fuzzy_filter() {
        let jobs = jobs_map(vec![
            job(1, "ubuntu-24.04-desktop.iso", JobState::Active),
            job(2, "debian-12.iso", JobState::Active),
            job(3, "Big Buck Bunny.mp4", JobState::Waiting),
        ]);
        let mut l = list(&jobs);
        l.filter = "ubuntu".into();
        l.recompute(&jobs);
        assert_eq!(l.order, vec![JobId(1)]);
        l.filter = "bbb".into();
        l.recompute(&jobs);
        assert_eq!(l.order, vec![JobId(3)]);
        l.filter = "zzz".into();
        l.recompute(&jobs);
        assert!(l.order.is_empty());
        assert_eq!(l.cursor, None);
    }

    #[test]
    fn cursor_survives_recompute_and_removal() {
        let mut jobs = jobs_map((1..=4).map(|i| job(i, "x", JobState::Active)).collect());
        let mut l = list(&jobs);
        l.move_by(2);
        assert_eq!(l.cursor, Some(JobId(3)));
        jobs.remove(&JobId(3));
        l.recompute(&jobs);
        assert_eq!(l.cursor, Some(JobId(4)));
        l.move_by(-10);
        assert_eq!(l.cursor, Some(JobId(1)));
        l.move_by(10);
        assert_eq!(l.cursor, Some(JobId(4)));
    }

    #[test]
    fn selection_and_range() {
        let jobs = jobs_map((1..=5).map(|i| job(i, "x", JobState::Active)).collect());
        let mut l = list(&jobs);
        assert_eq!(l.targets(), vec![JobId(1)]);
        l.move_by(1);
        l.toggle_cursor();
        l.move_by(2);
        l.select_range();
        assert_eq!(l.targets(), vec![JobId(2), JobId(3), JobId(4)]);
        l.select_none();
        assert_eq!(l.targets(), vec![JobId(4)]);
        l.select_all();
        assert_eq!(l.targets().len(), 5);
    }

    #[test]
    fn range_without_anchor_sets_it() {
        let jobs = jobs_map((1..=3).map(|i| job(i, "x", JobState::Active)).collect());
        let mut l = list(&jobs);
        l.select_range();
        assert_eq!(l.anchor, Some(JobId(1)));
        l.to_bottom();
        l.select_range();
        assert_eq!(l.selected.len(), 3);
    }

    #[test]
    fn stopped_section_is_paged() {
        let jobs = jobs_map((1..=7).map(|i| job(i, "x", JobState::Complete)).collect());
        let mut l = DownloadsList {
            page_size: 3,
            ..DownloadsList::default()
        };
        l.recompute(&jobs);
        assert_eq!(l.order.len(), 3);
        assert_eq!(l.stopped_pages(), 3);
        l.stopped_page = 2;
        l.recompute(&jobs);
        assert_eq!(l.order.len(), 1);
        l.stopped_page = 9;
        l.recompute(&jobs);
        assert_eq!(l.stopped_page, 2);
    }

    #[test]
    fn ensure_visible_scrolls() {
        let jobs = jobs_map((1..=30).map(|i| job(i, "x", JobState::Active)).collect());
        let mut l = list(&jobs);
        l.to_bottom();
        l.ensure_visible(10);
        assert_eq!(l.offset, 21);
        l.to_top();
        l.ensure_visible(10);
        assert_eq!(l.offset, 0);
    }
}
