//! A whole BMAD run as its orchestrator reports it: the path it takes, the planning documents,
//! the stories, the loop that builds and reviews each story, and the retrospective.
//!
//! A run takes one of three paths, each with its fixed steps ([`Run::steps`]): a single change
//! is specced and built; an epic is specced, its stories go through the loop and the epic ends
//! in a retrospective; a project writes its planning documents (a PRD and an architecture, and a
//! brief, a PRFAQ or a UX design once one of those is reported), breaks the work into epics and
//! stories, plans the sprint, runs the loop and ends in a retrospective.
//!
//! Starting a run starts the whole BMAD state over, with the run at its path's first step.
//! Starting a planning document records it and makes it the current step; recording the story
//! plan stores the epics and, on the project path, moves to the epics step; sprint planning and
//! the retrospective are steps of their own. Starting a story moves to the loop (to the build on
//! the single path) with the story in its build phase, and makes the epic that lists the story
//! the current one. Its review moves it to the review phase; finishing it done counts it and
//! ends its phase, while changes requested send it back to the build. Finishing the run records
//! its outcome. A persona a report names becomes the run's persona; a report without one keeps
//! the previous persona. A run-level report before any run is started changes nothing, and no
//! run-level report opens or closes a gate.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::workflow::bmad_v6_12::{Event, State, StoryRef};

/// The path a BMAD run takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPath {
    /// A single change: a spec, then its build.
    Single,
    /// One epic: a spec, its stories, then a retrospective.
    Epic,
    /// A whole project: the planning documents, the epics and stories, sprint planning, the
    /// stories, then a retrospective.
    Project,
}

/// The planning documents of a project run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningDoc {
    /// A product brief, written only when the run reports one.
    Brief,
    /// A press release and its frequently asked questions, written only when the run reports
    /// one.
    Prfaq,
    /// The product requirements document.
    Prd,
    /// A UX design, written only when the run reports one.
    Ux,
    /// The architecture.
    Architecture,
}

impl PlanningDoc {
    /// Every planning document, in the order a project writes them.
    const ALL: [Self; 5] = [
        Self::Brief,
        Self::Prfaq,
        Self::Prd,
        Self::Ux,
        Self::Architecture,
    ];

    /// Whether a project writes the document only when the run reports it.
    fn optional(self) -> bool {
        matches!(self, Self::Brief | Self::Prfaq | Self::Ux)
    }
}

/// One epic of a run's story plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicPlan {
    /// The epic's id, such as `1`.
    pub id: String,
    /// What the epic is about.
    pub title: String,
    /// The epic's stories, in their order.
    pub stories: Vec<StoryRef>,
}

/// How a story of a run ended its review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryOutcome {
    /// The story is done.
    Done,
    /// The story goes back to its build.
    ChangesRequested,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// The run reached its end.
    Completed,
    /// The run was given up.
    Abandoned,
}

/// A step of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStep {
    /// A planning document is written.
    Planning(PlanningDoc),
    /// The spec is written.
    Spec,
    /// The single change is built.
    Build,
    /// The work is broken into epics and stories.
    Epics,
    /// The sprint is planned.
    Sprint,
    /// The stories are built and reviewed, one after another.
    Loop,
    /// The epic is looked back on.
    Retro,
}

/// The phase of the story in the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopPhase {
    /// The story is built.
    Build,
    /// The story is reviewed.
    Review,
}

/// A BMAD run, folded from its orchestrator's run-level [`Event`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// What the run is about.
    pub topic: String,
    /// The path it takes.
    pub path: RunPath,
    /// The planning documents it reported, in the order it first reported them.
    pub planning: Vec<PlanningDoc>,
    /// The step it stands at, `None` when it stands at none.
    pub current: Option<RunStep>,
    /// The epics and their stories, once planned.
    pub epics: Vec<EpicPlan>,
    /// The current epic's id.
    pub epic: Option<String>,
    /// The current story's id.
    pub story: Option<String>,
    /// The current story's file.
    pub story_path: Option<PathBuf>,
    /// When the current story started, in seconds since the Unix epoch.
    pub story_at: u64,
    /// The current story's phase, `None` when no story is in the loop.
    pub phase: Option<LoopPhase>,
    /// The ids of the stories that are done, in the order they were finished.
    pub done_stories: Vec<String>,
    /// The epic of the retrospective.
    pub retro_epic: Option<String>,
    /// The BMAD persona named last.
    pub persona: Option<String>,
    /// How the run ended, `None` while it runs.
    pub outcome: Option<RunOutcome>,
}

impl Run {
    /// A run on `topic` along `path`, at the path's first step.
    fn new(topic: String, path: RunPath) -> Self {
        let first = match path {
            RunPath::Single | RunPath::Epic => RunStep::Spec,
            RunPath::Project => RunStep::Planning(PlanningDoc::Prd),
        };
        Self {
            topic,
            path,
            planning: Vec::new(),
            current: Some(first),
            epics: Vec::new(),
            epic: None,
            story: None,
            story_path: None,
            story_at: 0,
            phase: None,
            done_stories: Vec::new(),
            retro_epic: None,
            persona: None,
            outcome: None,
        }
    }

    /// The run's steps: the fixed steps of its path, with the optional planning documents of
    /// a project once reported.
    #[must_use]
    pub fn steps(&self) -> Vec<RunStep> {
        match self.path {
            RunPath::Single => vec![RunStep::Spec, RunStep::Build],
            RunPath::Epic => vec![RunStep::Spec, RunStep::Loop, RunStep::Retro],
            RunPath::Project => PlanningDoc::ALL
                .into_iter()
                .filter(|doc| !doc.optional() || self.planning.contains(doc))
                .map(RunStep::Planning)
                .chain([
                    RunStep::Epics,
                    RunStep::Sprint,
                    RunStep::Loop,
                    RunStep::Retro,
                ])
                .collect(),
        }
    }

    /// Makes `persona` the run's persona, when the report named one.
    fn name(&mut self, persona: Option<&String>) {
        if let Some(persona) = persona {
            self.persona = Some(persona.clone());
        }
    }

    /// Folds the run-level `event`, reported at `at`; any other event changes nothing.
    fn apply(&mut self, event: &Event, at: u64) {
        match event {
            Event::StartPlanning { doc, persona } => {
                if !self.planning.contains(doc) {
                    self.planning.push(*doc);
                }
                self.current = Some(RunStep::Planning(*doc));
                self.name(persona.as_ref());
            }
            Event::PlanStories { epics } => {
                self.epics.clone_from(epics);
                if self.path == RunPath::Project {
                    self.current = Some(RunStep::Epics);
                }
            }
            Event::StartSprintPlanning { persona } => {
                self.current = Some(RunStep::Sprint);
                self.name(persona.as_ref());
            }
            Event::StartEpic { id } => self.epic = Some(id.clone()),
            Event::StartStory { id, path, persona } => {
                self.current = Some(match self.path {
                    RunPath::Single => RunStep::Build,
                    RunPath::Epic | RunPath::Project => RunStep::Loop,
                });
                if let Some(epic) = self
                    .epics
                    .iter()
                    .find(|epic| epic.stories.iter().any(|story| story.id == *id))
                {
                    self.epic = Some(epic.id.clone());
                }
                self.story = Some(id.clone());
                self.story_path.clone_from(path);
                self.story_at = at;
                self.phase = Some(LoopPhase::Build);
                self.name(persona.as_ref());
            }
            Event::StartStoryReview { persona, .. } => {
                self.phase = Some(LoopPhase::Review);
                self.name(persona.as_ref());
            }
            Event::FinishStory { id, outcome } => match outcome {
                StoryOutcome::Done => {
                    if !self.done_stories.contains(id) {
                        self.done_stories.push(id.clone());
                    }
                    self.phase = None;
                }
                StoryOutcome::ChangesRequested => self.phase = Some(LoopPhase::Build),
            },
            Event::StartRetrospective { epic, persona } => {
                self.current = Some(RunStep::Retro);
                self.retro_epic = Some(epic.clone());
                self.name(persona.as_ref());
            }
            Event::FinishRun { outcome } => self.outcome = Some(*outcome),
            _ => {}
        }
    }
}

impl Event {
    /// Whether the event reports on a whole run, which only an orchestrator does.
    #[must_use]
    pub fn run_level(&self) -> bool {
        matches!(
            self,
            Self::StartRun { .. }
                | Self::StartPlanning { .. }
                | Self::PlanStories { .. }
                | Self::StartSprintPlanning { .. }
                | Self::StartEpic { .. }
                | Self::StartStory { .. }
                | Self::StartStoryReview { .. }
                | Self::FinishStory { .. }
                | Self::StartRetrospective { .. }
                | Self::FinishRun { .. }
        )
    }
}

/// Folds the run-level `event`, reported at `at`, into `state`: starting a run replaces any
/// run, and every other report changes the run once one started.
pub(super) fn apply(state: &mut State, event: &Event, at: u64) {
    if let Event::StartRun { topic, path } = event {
        state.run = Some(Run::new(topic.clone(), *path));
    } else if let Some(run) = &mut state.run {
        run.apply(event, at);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;
    use crate::workflow::bmad_v6_12::BmadV6_12;
    use crate::workflow::bmad_v6_12::WorkflowKind;
    use crate::workflow::{GateChange, WorkflowModule as _};

    /// The state after `events`, each reported at its index.
    fn fold(events: &[Event]) -> State {
        let mut state = State::default();
        for (at, event) in (1..).zip(events) {
            if BmadV6_12::starts_over(event) {
                state = State::default();
            }
            assert_eq!(BmadV6_12::apply(&mut state, event, at), GateChange::Keep);
        }
        state
    }

    fn start(path: RunPath) -> Event {
        Event::StartRun {
            topic: "Invoices".into(),
            path,
        }
    }

    fn planning(doc: PlanningDoc) -> Event {
        Event::StartPlanning { doc, persona: None }
    }

    fn the_run(state: &State) -> &Run {
        state.run.as_ref().unwrap()
    }

    fn story(id: &str) -> StoryRef {
        StoryRef {
            id: id.into(),
            title: format!("Story {id}"),
        }
    }

    fn plan() -> Event {
        Event::PlanStories {
            epics: vec![
                EpicPlan {
                    id: "1".into(),
                    title: "Billing".into(),
                    stories: vec![story("1.1"), story("1.2")],
                },
                EpicPlan {
                    id: "2".into(),
                    title: "Reports".into(),
                    stories: vec![story("2.1")],
                },
            ],
        }
    }

    fn start_story(id: &str) -> Event {
        Event::StartStory {
            id: id.into(),
            path: Some(format!("stories/{id}.md").into()),
            persona: Some("Amelia · dev".into()),
        }
    }

    #[test]
    fn each_path_has_its_fixed_steps() {
        use PlanningDoc::{Architecture, Prd};
        use RunStep::{Build, Epics, Loop, Planning, Retro, Spec, Sprint};
        let cases = [
            (RunPath::Single, vec![Spec, Build], Spec),
            (RunPath::Epic, vec![Spec, Loop, Retro], Spec),
            (
                RunPath::Project,
                vec![
                    Planning(Prd),
                    Planning(Architecture),
                    Epics,
                    Sprint,
                    Loop,
                    Retro,
                ],
                Planning(Prd),
            ),
        ];
        for (path, steps, first) in cases {
            let state = fold(&[start(path)]);
            let run = the_run(&state);
            assert_eq!(run.steps(), steps, "{path:?}");
            assert_eq!(run.current, Some(first), "{path:?}");
            assert_eq!((run.topic.as_str(), run.path), ("Invoices", path));
            assert_eq!(state.workflow, None);
        }
    }

    #[test]
    fn reported_optional_planning_docs_join_the_project_rail() {
        use PlanningDoc::{Architecture, Brief, Prd, Prfaq, Ux};
        use RunStep::{Epics, Loop, Planning, Retro, Sprint};
        let state = fold(&[
            start(RunPath::Project),
            planning(Ux),
            Event::StartPlanning {
                doc: Brief,
                persona: Some("Mary · analyst".into()),
            },
            planning(Brief),
        ]);
        let run = the_run(&state);
        assert_eq!(
            run.steps(),
            [
                Planning(Brief),
                Planning(Prd),
                Planning(Ux),
                Planning(Architecture),
                Epics,
                Sprint,
                Loop,
                Retro,
            ]
        );
        assert_eq!(run.planning, [Ux, Brief]);
        assert_eq!(run.current, Some(Planning(Brief)));
        // A report without a persona keeps the one named before.
        assert_eq!(run.persona.as_deref(), Some("Mary · analyst"));

        let prfaq = fold(&[start(RunPath::Project), planning(Prfaq), planning(Prd)]);
        assert_eq!(
            the_run(&prfaq).steps()[..3],
            [Planning(Prfaq), Planning(Prd), Planning(Architecture)]
        );
        assert_eq!(the_run(&prfaq).current, Some(Planning(Prd)));

        // The fixed paths take no planning steps.
        let epic = fold(&[start(RunPath::Epic), planning(Ux)]);
        assert_eq!(the_run(&epic).steps(), [RunStep::Spec, Loop, Retro]);
    }

    #[test]
    fn planning_the_stories_and_the_sprint_move_a_project_on() {
        let project = fold(&[start(RunPath::Project), plan()]);
        assert_eq!(the_run(&project).current, Some(RunStep::Epics));
        assert_eq!(the_run(&project).epics.len(), 2);
        assert_eq!(the_run(&project).epics[0].stories[1].id, "1.2");

        let epic = fold(&[start(RunPath::Epic), plan()]);
        assert_eq!(the_run(&epic).current, Some(RunStep::Spec));
        assert_eq!(the_run(&epic).epics.len(), 2);

        let sprint = fold(&[
            start(RunPath::Project),
            plan(),
            Event::StartSprintPlanning {
                persona: Some("Bob · sm".into()),
            },
        ]);
        assert_eq!(the_run(&sprint).current, Some(RunStep::Sprint));
        assert_eq!(the_run(&sprint).persona.as_deref(), Some("Bob · sm"));

        let retro = fold(&[
            start(RunPath::Epic),
            Event::StartRetrospective {
                epic: "1".into(),
                persona: Some("Bob · sm".into()),
            },
        ]);
        assert_eq!(the_run(&retro).current, Some(RunStep::Retro));
        assert_eq!(the_run(&retro).retro_epic.as_deref(), Some("1"));
        assert_eq!(the_run(&retro).persona.as_deref(), Some("Bob · sm"));
    }

    #[test]
    fn stories_move_through_build_and_review_and_back_on_changes() {
        let mut events = vec![
            start(RunPath::Project),
            plan(),
            Event::StartEpic { id: "1".into() },
            start_story("2.1"),
        ];
        let state = fold(&events);
        let run = the_run(&state);
        assert_eq!(run.current, Some(RunStep::Loop));
        // The epic that lists the story becomes the current one.
        assert_eq!(run.epic.as_deref(), Some("2"));
        assert_eq!(run.story.as_deref(), Some("2.1"));
        assert_eq!(run.story_path, Some("stories/2.1.md".into()));
        assert_eq!(run.story_at, 4);
        assert_eq!(run.phase, Some(LoopPhase::Build));
        assert_eq!(run.persona.as_deref(), Some("Amelia · dev"));

        events.push(Event::StartStoryReview {
            id: "2.1".into(),
            persona: Some("Quinn · qa".into()),
        });
        let review = fold(&events);
        assert_eq!(the_run(&review).phase, Some(LoopPhase::Review));
        assert_eq!(the_run(&review).persona.as_deref(), Some("Quinn · qa"));

        events.push(Event::FinishStory {
            id: "2.1".into(),
            outcome: StoryOutcome::ChangesRequested,
        });
        let changes = fold(&events);
        assert_eq!(the_run(&changes).phase, Some(LoopPhase::Build));
        assert!(the_run(&changes).done_stories.is_empty());

        events.push(Event::FinishStory {
            id: "2.1".into(),
            outcome: StoryOutcome::Done,
        });
        let done = fold(&events);
        assert_eq!(the_run(&done).phase, None);
        assert_eq!(the_run(&done).done_stories, ["2.1"]);

        // A story no epic lists keeps the current epic.
        let unlisted = fold(&[
            start(RunPath::Project),
            plan(),
            Event::StartEpic { id: "1".into() },
            start_story("9.9"),
        ]);
        assert_eq!(the_run(&unlisted).epic.as_deref(), Some("1"));

        // On the single path a story is the build.
        let single = fold(&[start(RunPath::Single), start_story("1")]);
        assert_eq!(the_run(&single).current, Some(RunStep::Build));
        assert_eq!(the_run(&single).phase, Some(LoopPhase::Build));
    }

    #[test]
    fn finish_run_records_the_outcome() {
        for outcome in [RunOutcome::Completed, RunOutcome::Abandoned] {
            let state = fold(&[start(RunPath::Single), Event::FinishRun { outcome }]);
            assert_eq!(the_run(&state).outcome, Some(outcome));
        }
        assert_eq!(the_run(&fold(&[start(RunPath::Single)])).outcome, None);
    }

    #[test]
    fn a_report_of_no_run_step_leaves_the_run_alone() {
        let state = fold(&[start(RunPath::Epic)]);
        let mut run = the_run(&state).clone();
        run.apply(&Event::FinishWorkflow { outcome: None }, 2);
        run.apply(&start(RunPath::Project), 3);
        assert_eq!(&run, the_run(&state));
    }

    #[test]
    fn a_run_report_before_any_run_changes_nothing() {
        let state = fold(&[plan(), start_story("1.1")]);
        assert_eq!(state, State::default());
    }

    #[test]
    fn starting_a_run_or_a_workflow_starts_the_bmad_state_over() {
        let workflow = Event::StartWorkflow {
            kind: WorkflowKind::Build,
            persona: None,
            story: None,
            epic: None,
        };
        let run = fold(&[workflow.clone(), start(RunPath::Epic)]);
        assert_eq!(run.workflow, None);
        assert!(run.run.is_some());

        let replaced = fold(&[start(RunPath::Epic), workflow]);
        assert_eq!(replaced.run, None);
        assert!(replaced.workflow.is_some());
    }

    #[test]
    fn only_run_reports_are_run_level() {
        let run_level = [
            start(RunPath::Single),
            planning(PlanningDoc::Prd),
            plan(),
            Event::StartSprintPlanning { persona: None },
            Event::StartEpic { id: "1".into() },
            start_story("1.1"),
            Event::StartStoryReview {
                id: "1.1".into(),
                persona: None,
            },
            Event::FinishStory {
                id: "1.1".into(),
                outcome: StoryOutcome::Done,
            },
            Event::StartRetrospective {
                epic: "1".into(),
                persona: None,
            },
            Event::FinishRun {
                outcome: RunOutcome::Completed,
            },
        ];
        for event in &run_level {
            assert!(event.run_level(), "{event:?}");
            assert_eq!(BmadV6_12::tool_answer(event), crate::workflow::RECORDED);
        }
        let workflow = [
            Event::StartWorkflow {
                kind: WorkflowKind::Build,
                persona: None,
                story: None,
                epic: None,
            },
            Event::FinishWorkflow { outcome: None },
        ];
        for event in &workflow {
            assert!(!event.run_level(), "{event:?}");
        }
    }

    #[test]
    fn a_run_round_trips() {
        let state = fold(&[
            start(RunPath::Project),
            planning(PlanningDoc::Ux),
            plan(),
            start_story("1.1"),
            Event::FinishRun {
                outcome: RunOutcome::Abandoned,
            },
        ]);
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&json).unwrap(), state);
        // A state stored before runs were reported still loads.
        assert_eq!(
            serde_json::from_str::<State>(r#"{"workflow": null}"#).unwrap(),
            State::default()
        );
    }
}
