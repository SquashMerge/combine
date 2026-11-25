//! The six tools: their arguments, the wait for a verdict, the remove read-back, the results
//! and the audit line.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use combine_protocol::{CreatedBy, PanelError, PanelState, RenderStatus, Renderer};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::api::PanelApi;
use super::jsonrpc::{clip, fits, present, quoted, SERDE_TEXT_MAX};
use super::{tool_error_code, McpServer};
use crate::core::{
    ClientRender, PanelApiError, PanelCreate, PanelUpdate, PanelView, PANEL_ID_MAX_BYTES,
};
use crate::model::Lifetime;

const LATER_MILESTONES_SUFFIX: &str = "create_panel makes static Mermaid and Markdown panels";
const REMOVE_CONFLICT: &str = "the panel's files changed outside Combine, so it wasn't removed";

#[derive(Clone, Copy)]
pub(super) enum Tool {
    Create,
    Update,
    Show,
    Status,
    List,
    Remove,
}

impl Tool {
    pub(super) fn from_name(name: &str) -> Option<Tool> {
        match name {
            "create_panel" => Some(Tool::Create),
            "update_panel" => Some(Tool::Update),
            "show_diagram" => Some(Tool::Show),
            "panel_status" => Some(Tool::Status),
            "list_panels" => Some(Tool::List),
            "remove_panel" => Some(Tool::Remove),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Tool::Create => "create_panel",
            Tool::Update => "update_panel",
            Tool::Show => "show_diagram",
            Tool::Status => "panel_status",
            Tool::List => "list_panels",
            Tool::Remove => "remove_panel",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs<'a> {
    renderer: Option<String>,
    content: Option<String>,
    title: Option<String>,
    id: Option<String>,
    lifetime: Option<String>,
    idempotency_key: Option<String>,
    placement: Option<String>,
    #[serde(borrow, default, deserialize_with = "present")]
    kind: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present")]
    group: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present")]
    loader: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present")]
    sources: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present")]
    refresh: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present")]
    params: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    panel: Option<String>,
    content: Option<String>,
    #[serde(default, deserialize_with = "present")]
    title: Option<Option<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowArgs {
    source: Option<String>,
    title: Option<String>,
    id: Option<String>,
    idempotency_key: Option<String>,
    placement: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArgs {
    panel: Option<String>,
    revision: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveArgs {
    panel: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {}

/// A panel as a tool result shows it. The field order is part of the contract.
#[derive(Serialize)]
struct PanelResult<'a> {
    panel: &'a str,
    revision: u64,
    state: PanelState,
    error: Option<&'a PanelError>,
    renderer: Renderer,
    lifetime: &'static str,
    title: Option<&'a str>,
    placement: Option<&'a str>,
    created_by: Option<&'a CreatedBy>,
    file_error: Option<&'a str>,
    read_only: bool,
    superseded: bool,
    /// Set only by `panel_status`; every other tool's result omits it.
    #[serde(skip_serializing_if = "Option::is_none")]
    render_status: Option<Vec<RenderEntry<'a>>>,
}

/// What one client reported drawing of a panel.
#[derive(Serialize)]
struct RenderEntry<'a> {
    revision: u64,
    status: RenderStatus,
    error: Option<&'a PanelError>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorDetail<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    panel: Option<ConflictPanel<'a>>,
}

#[derive(Serialize)]
struct ErrorDetail<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'a str>,
}

#[derive(Serialize)]
struct ConflictPanel<'a> {
    id: &'a str,
    revision: u64,
    state: PanelState,
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

fn lifetime_name(lifetime: Lifetime) -> &'static str {
    match lifetime {
        Lifetime::Persistent => "persistent",
        Lifetime::Scratch => "scratch",
    }
}

fn state_name(state: PanelState) -> &'static str {
    match state {
        PanelState::Validating => "validating",
        PanelState::Fetching => "fetching",
        PanelState::Validated => "validated",
        PanelState::ValidatedParserOnly => "validated_parser_only",
        PanelState::Failed => "failed",
        PanelState::Untrusted => "untrusted",
        PanelState::Removed => "removed",
    }
}

fn panel_json(view: &PanelView, superseded: bool, renders: Option<&[ClientRender]>) -> String {
    to_json(&PanelResult {
        panel: &view.id,
        revision: view.revision,
        state: view.state,
        error: view.error.as_ref(),
        renderer: view.file.renderer,
        lifetime: lifetime_name(view.lifetime),
        title: view.file.title.as_deref(),
        placement: view.file.placement.as_deref(),
        created_by: view.file.created_by.as_ref(),
        file_error: view.file_error.as_deref(),
        read_only: view.read_only,
        superseded,
        render_status: renders.map(|renders| {
            renders
                .iter()
                .map(|render| RenderEntry {
                    revision: render.revision,
                    status: render.status,
                    error: render.error.as_ref(),
                })
                .collect()
        }),
    })
}

/// What a tool call answers: the text of the result and whether it is an error.
struct Reply {
    text: String,
    is_error: bool,
}

impl Reply {
    fn panel(view: &PanelView, superseded: bool) -> Reply {
        Reply::panel_with(view, superseded, None)
    }

    fn panel_with(view: &PanelView, superseded: bool, renders: Option<&[ClientRender]>) -> Reply {
        Reply {
            text: panel_json(view, superseded, renders),
            is_error: false,
        }
    }

    /// The `tools/call` result JSON.
    fn result(&self) -> String {
        format!(
            r#"{{"content":[{{"type":"text","text":{}}}],"isError":{}}}"#,
            quoted(&self.text),
            self.is_error
        )
    }
}

struct ToolError {
    code: &'static str,
    message: String,
    field: Option<&'static str>,
    conflict: Option<(String, u64, PanelState)>,
}

impl ToolError {
    fn new(code: &'static str, message: impl Into<String>) -> ToolError {
        ToolError {
            code,
            message: message.into(),
            field: None,
            conflict: None,
        }
    }

    fn invalid(message: impl Into<String>) -> ToolError {
        ToolError::new("invalid_arguments", message)
    }

    fn not_supported(message: impl Into<String>) -> ToolError {
        ToolError::new("not_supported", message)
    }

    fn required(name: &str) -> ToolError {
        ToolError::invalid(format!("{name} is required"))
    }

    fn api(error: &PanelApiError) -> ToolError {
        ToolError {
            code: tool_error_code(error),
            message: error.to_string(),
            field: match error {
                PanelApiError::InvalidField { field, .. } => Some(field),
                _ => None,
            },
            conflict: None,
        }
    }

    fn reply(&self) -> Reply {
        let panel = self
            .conflict
            .as_ref()
            .map(|(id, revision, state)| ConflictPanel {
                id,
                revision: *revision,
                state: *state,
            });
        Reply {
            text: to_json(&ErrorBody {
                error: ErrorDetail {
                    code: self.code,
                    message: &self.message,
                    field: self.field,
                },
                panel,
            }),
            is_error: true,
        }
    }
}

/// How a mutating call ended, for its audit line.
enum End {
    View { revision: u64, state: PanelState },
    Error(&'static str),
}

struct Entry {
    panel: Option<String>,
    end: End,
}

/// A tool call's answer (`None` when its connection closed first) and its audit entry.
type Outcome = (Option<Reply>, Entry);

fn refused(error: ToolError, panel: Option<String>) -> Outcome {
    let entry = Entry {
        panel,
        end: End::Error(error.code),
    };
    (Some(error.reply()), entry)
}

fn entry_of(view: &PanelView) -> Entry {
    Entry {
        panel: Some(view.id.clone()),
        end: End::View {
            revision: view.revision,
            state: view.state,
        },
    }
}

fn answered(view: &PanelView, superseded: bool) -> Outcome {
    (Some(Reply::panel(view, superseded)), entry_of(view))
}

fn interrupted(view: &PanelView) -> Outcome {
    (None, entry_of(view))
}

fn parse_args<'a, T: Deserialize<'a>>(raw: Option<&'a RawValue>) -> Result<T, ToolError> {
    let text = raw.map_or("{}", |raw| raw.get());
    if !text.trim_start().starts_with('{') {
        return Err(ToolError::invalid("arguments must be an object"));
    }
    serde_json::from_str(text)
        .map_err(|error| ToolError::invalid(clip(&error.to_string(), SERDE_TEXT_MAX)))
}

fn later_milestone(args: &CreateArgs<'_>) -> Option<ToolError> {
    let refusals: [(bool, &str); 6] = [
        (
            args.kind.is_some(),
            "kind panels come with milestone M4 (panel kinds and the view language)",
        ),
        (args.group.is_some(), "panel groups come with milestone M4"),
        (
            args.loader.is_some(),
            "loaders come with milestone M3 (loaders and data panels)",
        ),
        (
            args.sources.is_some(),
            "live sources come with milestone M3 (loaders and data panels)",
        ),
        (
            args.refresh.is_some(),
            "refresh policies come with milestone M3 (loaders and data panels)",
        ),
        (
            args.params.is_some(),
            "panel params come with milestone M3 (loaders and data panels)",
        ),
    ];
    refusals
        .iter()
        .find(|(present, _)| *present)
        .map(|(_, what)| ToolError::not_supported(format!("{what}; {LATER_MILESTONES_SUFFIX}")))
}

impl McpServer {
    pub(super) fn run_tool(
        &self,
        tool: Tool,
        arguments: Option<&RawValue>,
        api: &dyn PanelApi,
        closed: &AtomicBool,
        id: &str,
    ) -> Option<String> {
        let (reply, entry) = match tool {
            Tool::Create => self.create(arguments, api, closed),
            Tool::Update => self.update(arguments, api, closed),
            Tool::Show => self.show(arguments, api, closed),
            Tool::Remove => self.remove(arguments, api, closed),
            Tool::Status => return Some(self.status(arguments, api).result()),
            Tool::List => return Some(self.list(arguments, api, id)),
        };
        self.audit_call(tool, &entry);
        reply.map(|reply| reply.result())
    }

    fn audit_call(&self, tool: Tool, entry: &Entry) {
        let slot = self
            .caller
            .slot
            .as_deref()
            .map_or_else(|| "null".to_owned(), |slot| format!("{slot:?}"));
        let panel = entry.panel.as_deref().map_or_else(
            || "null".to_owned(),
            |panel| format!("{:?}", clip(panel, PANEL_ID_MAX_BYTES)),
        );
        let end = match &entry.end {
            End::View { revision, state } => {
                format!("revision={revision} state={}", state_name(*state))
            }
            End::Error(code) => format!("error={code}"),
        };
        self.audit.record(&format!(
            "mcp {} space={:?} slot={slot} panel={panel} {end}",
            tool.name(),
            self.caller.space.0
        ));
    }

    fn create(
        &self,
        arguments: Option<&RawValue>,
        api: &dyn PanelApi,
        closed: &AtomicBool,
    ) -> Outcome {
        let args: CreateArgs<'_> = match parse_args(arguments) {
            Ok(args) => args,
            Err(error) => return refused(error, None),
        };
        let named = args.id.clone();
        if let Some(error) = later_milestone(&args) {
            return refused(error, named);
        }
        let renderer = match args.renderer.as_deref() {
            None => {
                return refused(
                    ToolError::invalid(r#"renderer is required: "mermaid" or "markdown""#),
                    named,
                )
            }
            Some("mermaid") => Renderer::Mermaid,
            Some("markdown") => Renderer::Markdown,
            Some("terminal") => {
                return refused(
                    ToolError::not_supported(
                        "terminals can't be created through MCP; the user starts terminals from Add terminal",
                    ),
                    named,
                )
            }
            Some(_) => {
                return refused(
                    ToolError::invalid(r#"renderer must be "mermaid" or "markdown""#),
                    named,
                )
            }
        };
        let Some(content) = args.content else {
            return refused(ToolError::required("content"), named);
        };
        let lifetime = match args.lifetime.as_deref() {
            None | Some("persistent") => Lifetime::Persistent,
            Some("scratch") => Lifetime::Scratch,
            Some(_) => {
                return refused(
                    ToolError::invalid(r#"lifetime must be "persistent" or "scratch""#),
                    named,
                )
            }
        };
        let request = PanelCreate {
            id: args.id,
            lifetime,
            renderer,
            title: args.title,
            idempotency_key: args.idempotency_key,
            created_by: Some(self.created_by()),
            placement: args.placement,
            content,
        };
        self.create_and_wait(request, named, api, closed)
    }

    fn show(
        &self,
        arguments: Option<&RawValue>,
        api: &dyn PanelApi,
        closed: &AtomicBool,
    ) -> Outcome {
        let args: ShowArgs = match parse_args(arguments) {
            Ok(args) => args,
            Err(error) => return refused(error, None),
        };
        let named = args.id.clone();
        let Some(content) = args.source else {
            return refused(ToolError::required("source"), named);
        };
        let request = PanelCreate {
            id: args.id,
            lifetime: Lifetime::Scratch,
            renderer: Renderer::Mermaid,
            title: args.title,
            idempotency_key: args.idempotency_key,
            created_by: Some(self.created_by()),
            placement: args.placement,
            content,
        };
        self.create_and_wait(request, named, api, closed)
    }

    fn created_by(&self) -> CreatedBy {
        CreatedBy::Agent {
            slot: self.caller.slot.clone(),
        }
    }

    fn create_and_wait(
        &self,
        request: PanelCreate,
        named: Option<String>,
        api: &dyn PanelApi,
        closed: &AtomicBool,
    ) -> Outcome {
        match api.create(request) {
            Ok(view) => self.wait(view, api, closed),
            Err(error) => refused(ToolError::api(&error), named),
        }
    }

    fn update(
        &self,
        arguments: Option<&RawValue>,
        api: &dyn PanelApi,
        closed: &AtomicBool,
    ) -> Outcome {
        let args: UpdateArgs = match parse_args(arguments) {
            Ok(args) => args,
            Err(error) => return refused(error, None),
        };
        let Some(panel) = args.panel else {
            return refused(ToolError::required("panel"), None);
        };
        let update = PanelUpdate {
            content: args.content,
            title: args.title,
        };
        match api.update(&panel, update) {
            Ok(view) => self.wait(view, api, closed),
            Err(error) => refused(ToolError::api(&error), Some(panel)),
        }
    }

    /// Reads the panel again at once, then every poll, until its state isn't `validating` or the
    /// wait is over. The first read is what makes the answer true when the call gave way to an
    /// outside edit. The closed flag is checked only between panel calls.
    fn wait(&self, first: PanelView, api: &dyn PanelApi, closed: &AtomicBool) -> Outcome {
        let (id, revision) = (first.id.clone(), first.revision);
        let deadline = Instant::now() + self.options.wait;
        let mut last = first;
        loop {
            if closed.load(Ordering::SeqCst) {
                return interrupted(&last);
            }
            let view = match api.status(&id) {
                Ok(view) => view,
                Err(error) => return refused(ToolError::api(&error), Some(id)),
            };
            if view.revision != revision {
                return answered(&view, true);
            }
            let now = Instant::now();
            if view.state != PanelState::Validating || now >= deadline {
                return answered(&view, false);
            }
            last = view;
            if closed.load(Ordering::SeqCst) {
                return interrupted(&last);
            }
            std::thread::sleep(self.options.poll.min(deadline - now));
        }
    }

    fn remove(
        &self,
        arguments: Option<&RawValue>,
        api: &dyn PanelApi,
        closed: &AtomicBool,
    ) -> Outcome {
        let args: RemoveArgs = match parse_args(arguments) {
            Ok(args) => args,
            Err(error) => return refused(error, None),
        };
        let Some(panel) = args.panel else {
            return refused(ToolError::required("panel"), None);
        };
        let removed = match api.remove(&panel) {
            Ok(view) => view,
            Err(error) => return refused(ToolError::api(&error), Some(panel)),
        };
        if removed.lifetime == Lifetime::Scratch {
            return answered(&removed, false);
        }
        if closed.load(Ordering::SeqCst) {
            return interrupted(&removed);
        }
        match api.status(&panel) {
            Err(error) => refused(ToolError::api(&error), Some(panel)),
            Ok(view) if view.state == PanelState::Removed => answered(&view, false),
            Ok(view) => {
                let mut error = ToolError::new("remove_conflict", REMOVE_CONFLICT);
                error.conflict = Some((view.id.clone(), view.revision, view.state));
                refused(error, Some(view.id))
            }
        }
    }

    fn status(&self, arguments: Option<&RawValue>, api: &dyn PanelApi) -> Reply {
        let args: StatusArgs = match parse_args(arguments) {
            Ok(args) => args,
            Err(error) => return error.reply(),
        };
        let Some(panel) = args.panel else {
            return ToolError::required("panel").reply();
        };
        let view = match api.status(&panel) {
            Ok(view) => view,
            Err(error) => return ToolError::api(&error).reply(),
        };
        match args.revision {
            Some(asked) if asked > view.revision => ToolError::new(
                "unknown_revision",
                format!(
                    "the panel {panel} has no revision {asked}; its current revision is {}",
                    view.revision
                ),
            )
            .reply(),
            asked => match api.render_status(&panel) {
                Ok(renders) => Reply::panel_with(
                    &view,
                    asked.is_some_and(|asked| asked < view.revision),
                    Some(&renders),
                ),
                Err(error) => ToolError::api(&error).reply(),
            },
        }
    }

    /// The result JSON of `list_panels`: the largest prefix of the panels, sorted by id, that
    /// fits one frame.
    fn list(&self, arguments: Option<&RawValue>, api: &dyn PanelApi, id: &str) -> String {
        if let Err(error) = parse_args::<ListArgs>(arguments) {
            return error.reply().result();
        }
        let list = match api.list() {
            Ok(list) => list,
            Err(error) => return ToolError::api(&error).reply().result(),
        };
        let dropped = match api.layout_dropped() {
            Ok(Some(notice)) => to_json(&notice),
            Ok(None) => "null".to_owned(),
            Err(error) => return ToolError::api(&error).reply().result(),
        };
        let mut views: Vec<&PanelView> = list.panels.iter().collect();
        views.sort_by(|a, b| a.id.cmp(&b.id));
        let panels: Vec<String> = views
            .iter()
            .map(|view| panel_json(view, false, None))
            .collect();
        let skipped = to_json(&list.skipped);
        let error = list
            .error
            .as_deref()
            .map_or_else(|| "null".to_owned(), quoted);
        let count = panels.len();
        let result_of = |kept: usize| {
            let text = format!(
                r#"{{"space":{},"panels":[{}],"skipped":{skipped},"error":{error},"dropped":{dropped},"count":{count},"truncated":{}}}"#,
                quoted(&self.caller.space.0),
                panels[..kept].join(","),
                kept < count
            );
            Reply {
                text,
                is_error: false,
            }
            .result()
        };
        let full = result_of(count);
        if fits(Some(id), &full) {
            return full;
        }
        let (mut fitting, mut over) = (0, count);
        while over - fitting > 1 {
            let middle = fitting + (over - fitting) / 2;
            if fits(Some(id), &result_of(middle)) {
                fitting = middle;
            } else {
                over = middle;
            }
        }
        result_of(fitting)
    }
}
