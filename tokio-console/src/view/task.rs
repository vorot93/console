use crate::{
    input,
    state::{tasks::Task, DetailsRef},
    util::Percentage,
    view::{
        self, bold,
        controls::{controls_paragraph, ControlDisplay, Controls, KeyDisplay},
        durations::Durations,
        help::HelpText,
    },
};
use ratatui::{
    layout::{self, Layout},
    text::{Line, Span, Text},
    widgets::{List, ListItem, Paragraph},
};
use std::{
    cell::RefCell,
    cmp,
    rc::Rc,
    time::{Duration, SystemTime},
};

pub(crate) struct TaskView {
    task: Rc<RefCell<Task>>,
    details: DetailsRef,
}

impl TaskView {
    pub(super) fn new(task: Rc<RefCell<Task>>, details: DetailsRef) -> Self {
        TaskView { task, details }
    }

    pub(crate) fn update_input(&mut self, _event: input::Event) {
        // TODO :D
    }

    pub(crate) fn render(
        &mut self,
        styles: &view::Styles,
        frame: &mut ratatui::terminal::Frame,
        area: layout::Rect,
        now: SystemTime,
    ) {
        // Rows with the following info:
        // - Task main attributes
        // - task metadata
        // - metrics
        // - logs?

        let task = &*self.task.borrow();
        let details_ref = self.details.borrow();
        let details = details_ref
            .as_ref()
            .filter(|details| details.span_id() == task.span_id());

        let controls = Controls::new(view_controls(), &area, styles);

        let warnings: Vec<_> = task
            .warnings()
            .iter()
            .map(|linter| {
                ListItem::new(Text::from(Line::from(vec![
                    styles.warning_wide(),
                    // TODO(eliza): it would be nice to handle singular vs plural...
                    Span::from(linter.format(task)),
                ])))
            })
            .collect();

        let (
            controls_area,
            stats_area,
            poll_dur_area,
            scheduled_dur_area,
            fields_area,
            warnings_area,
        ) = if warnings.is_empty() {
            let chunks = Layout::default()
                .direction(layout::Direction::Vertical)
                .constraints(
                    [
                        // controls
                        layout::Constraint::Length(controls.height()),
                        // task stats
                        layout::Constraint::Length(10),
                        // poll duration
                        layout::Constraint::Length(9),
                        // scheduled duration
                        layout::Constraint::Length(9),
                        // fields
                        layout::Constraint::Percentage(60),
                    ]
                    .as_ref(),
                )
                .split(area);
            (chunks[0], chunks[1], chunks[2], chunks[3], chunks[4], None)
        } else {
            let chunks = Layout::default()
                .direction(layout::Direction::Vertical)
                .constraints(
                    [
                        // controls
                        layout::Constraint::Length(controls.height()),
                        // warnings (add 2 for top and bottom borders)
                        layout::Constraint::Length(warnings.len() as u16 + 2),
                        // task stats
                        layout::Constraint::Length(10),
                        // poll duration
                        layout::Constraint::Length(9),
                        // scheduled duration
                        layout::Constraint::Length(9),
                        // fields
                        layout::Constraint::Percentage(60),
                    ]
                    .as_ref(),
                )
                .split(area);

            (
                chunks[0],
                chunks[2],
                chunks[3],
                chunks[4],
                chunks[5],
                Some(chunks[1]),
            )
        };

        let stats_area = Layout::default()
            .direction(layout::Direction::Horizontal)
            .constraints(
                [
                    layout::Constraint::Percentage(50),
                    layout::Constraint::Percentage(50),
                ]
                .as_ref(),
            )
            .split(stats_area);

        // Just preallocate capacity for ID, name, target, total, busy, and idle.
        let mut overview = Vec::with_capacity(8);
        overview.push(Line::from(vec![
            bold("ID: "),
            Span::raw(format!("{} ", task.id_str())),
            task.state().render(styles),
        ]));

        if let Some(name) = task.name() {
            overview.push(Line::from(vec![bold("Name: "), Span::raw(name)]));
        }

        overview.push(Line::from(vec![bold("Target: "), Span::raw(task.target())]));

        let title = "Location: ";
        let location_max_width = stats_area[0].width as usize - 2 - title.len(); // NOTE: -2 for the border
        let location = if task.location().len() > location_max_width {
            let ellipsis = styles.if_utf8("\u{2026}", "...");
            let start = task.location().len() - location_max_width + ellipsis.chars().count();
            format!("{}{}", ellipsis, &task.location()[start..])
        } else {
            task.location().to_string()
        };

        overview.push(Line::from(vec![bold(title), Span::raw(location)]));

        let total = task.total(now);

        let dur_percent = |name: &'static str, amt: Duration| -> Line {
            let percent = amt.as_secs_f64().percent_of(total.as_secs_f64());
            Line::from(vec![
                bold(name),
                styles.time_units(amt, view::DUR_LIST_PRECISION, None),
                Span::from(format!(" ({:.2}%)", percent)),
            ])
        };

        overview.push(Line::from(vec![
            bold("Total Time: "),
            styles.time_units(total, view::DUR_LIST_PRECISION, None),
        ]));
        overview.push(dur_percent("Busy: ", task.busy(now)));
        overview.push(dur_percent("Scheduled: ", task.scheduled(now)));
        overview.push(dur_percent("Idle: ", task.idle(now)));

        let mut waker_stats = vec![Line::from(vec![
            bold("Current wakers: "),
            Span::from(format!("{} (", task.waker_count())),
            bold("clones: "),
            Span::from(format!("{}, ", task.waker_clones())),
            bold("drops: "),
            Span::from(format!("{})", task.waker_drops())),
        ])];

        let mut wakeups = vec![
            bold("Woken: "),
            Span::from(format!("{} times", task.wakes())),
        ];

        // If the task has been woken, add the time since wake to its stats as well.
        if let Some(since) = task.since_wake(now) {
            wakeups.reserve(3);
            wakeups.push(Span::raw(", "));
            wakeups.push(bold("last woken: "));
            wakeups.push(styles.time_units(since, view::DUR_LIST_PRECISION, None));
            wakeups.push(Span::raw(" ago"));
        }

        waker_stats.push(Line::from(wakeups));

        if task.self_wakes() > 0 {
            waker_stats.push(Line::from(vec![
                bold("Self Wakes: "),
                Span::from(format!(
                    "{} times ({}%)",
                    task.self_wakes(),
                    task.self_wake_percent()
                )),
            ]));
        }

        let mut fields = Text::default();
        fields.extend(task.formatted_fields().iter().cloned().map(Line::from));

        if let Some(warnings_area) = warnings_area {
            let warnings = List::new(warnings).block(styles.border_block().title("Warnings"));
            frame.render_widget(warnings, warnings_area);
        }

        let task_widget = Paragraph::new(overview).block(styles.border_block().title("Task"));
        let wakers_widget = Paragraph::new(waker_stats).block(styles.border_block().title("Waker"));

        let poll_percentiles_title = "Poll Times Percentiles";
        let scheduled_percentiles_title = "Sched Times Percentiles";
        let percentiles_width = cmp::max(
            poll_percentiles_title.len(),
            scheduled_percentiles_title.len(),
        ) as u16
            + 2_u16; // extra 2 characters for the border
        let poll_durations_widget = Durations::new(styles)
            .histogram(details.and_then(|d| d.poll_times_histogram()))
            .percentiles_title(poll_percentiles_title)
            .histogram_title("Poll Times Histogram")
            .percentiles_width(percentiles_width);
        let scheduled_durations_widget = Durations::new(styles)
            .histogram(details.and_then(|d| d.scheduled_times_histogram()))
            .percentiles_title(scheduled_percentiles_title)
            .histogram_title("Scheduled Times Histogram")
            .percentiles_width(percentiles_width);

        let fields_widget = Paragraph::new(fields).block(styles.border_block().title("Fields"));

        frame.render_widget(controls.into_widget(), controls_area);
        frame.render_widget(task_widget, stats_area[0]);
        frame.render_widget(wakers_widget, stats_area[1]);
        frame.render_widget(poll_durations_widget, poll_dur_area);
        frame.render_widget(scheduled_durations_widget, scheduled_dur_area);
        frame.render_widget(fields_widget, fields_area);
    }
}

impl HelpText for TaskView {
    fn render_help_content(&self, styles: &view::Styles) -> Paragraph<'static> {
        controls_paragraph(view_controls(), styles)
    }
}

const fn view_controls() -> &'static [ControlDisplay] {
    &[ControlDisplay {
        action: "return to task list",
        keys: &[KeyDisplay {
            base: "esc",
            utf8: Some("\u{238B} esc"),
        }],
    }]
}
