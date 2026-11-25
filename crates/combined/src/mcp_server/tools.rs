//! The six tools: names, descriptions and input schemas, as `tools/list` offers them.

use super::TOOL_NAMES;

pub(super) struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// The input schema, as JSON text.
    pub input_schema: &'static str,
}

const CREATE_DESCRIPTION: &str = r#"Create a panel next to this terminal in the Combine app, in this terminal's space: a Mermaid diagram (renderer "mermaid") or a Markdown note (renderer "markdown"). Mermaid uses Mermaid 12.1.0 syntax and is checked by Pele 1.13.0, the renderer the app uses; Markdown is CommonMark without raw HTML. Content is static text: at most 204,800 bytes of UTF-8. Lifetime "persistent" (the default) saves the panel in the project as .workspace/panels/<id>/; "scratch" keeps it in the daemon's own data until 7 days after its last change. The call waits up to about 5 s for validation and returns the panel's id, revision and state: "validated", "failed" with an error (code, message, line, column, snippet; fix it with update_panel on the same id) or "validating" (still being checked; ask panel_status later). Idempotency: if a live panel of this space already has idempotency_key, the call returns that panel and changes nothing, even if id, content or renderer differ, also after a daemon restart; once that panel is removed or expires, the key makes a new panel. Placement: placement names a layout tile; it is stored with the panel and applied when a Combine client or the daemon places the panel, never as a new tile. Limits: 100 panels per space; id 1 to 255 bytes with no control characters, no / and no leading dot (omit it for panel-<n>); title 1 to 1,024 bytes; idempotency_key 1 to 256 bytes. Only static Mermaid and Markdown panels exist yet: kind and group come with milestone M4, loader, sources, refresh and params with M3, and terminals can't be created here."#;

const CREATE_SCHEMA: &str = r#"{"type":"object","properties":{"renderer":{"type":"string","enum":["mermaid","markdown"],"description":"mermaid for a Mermaid diagram (Mermaid 12.1.0 syntax), markdown for a Markdown note."},"content":{"type":"string","description":"The whole source: at most 204,800 bytes of UTF-8."},"title":{"type":"string","description":"The tab title: 1 to 1,024 bytes, no control characters."},"id":{"type":"string","description":"The panel ID: 1 to 255 bytes, no control characters, no /, not starting with a dot. Omit it and the daemon picks panel-<n>."},"lifetime":{"type":"string","enum":["persistent","scratch"],"description":"persistent (the default) saves the panel in the project's .workspace/panels/<id>/; scratch keeps it in the daemon's data until 7 days after its last change."},"idempotency_key":{"type":"string","description":"1 to 256 bytes. A repeated create with a key a live panel of this space has returns that panel."},"placement":{"type":"string","description":"The ID of the layout tile to show the panel in: a hint, stored with the panel and applied when a client or the daemon places it."}},"required":["renderer","content"],"additionalProperties":false}"#;

const UPDATE_DESCRIPTION: &str = r#"Change a panel's content or title, or both, and get its new revision and state. Send the whole new source as content: this is how to fix a failed diagram, on the same panel id. Like create_panel, the call waits up to about 5 s and answers "validated", "failed" (code, message, line, column, snippet) or "validating". Revisions only go up. title null clears the title. Content is at most 204,800 bytes of UTF-8; Mermaid uses Mermaid 12.1.0 syntax. Refused with removed for a removed panel, read_only when its folder is read-only, and unreadable while its files can't be read. If the panel's files changed outside the daemon while it saved, the outside edit wins: the result is the newer revision, with superseded true."#;

const UPDATE_SCHEMA: &str = r#"{"type":"object","properties":{"panel":{"type":"string","description":"The panel's ID, as create_panel or show_diagram returned it."},"content":{"type":"string","description":"The whole new source: at most 204,800 bytes of UTF-8."},"title":{"type":["string","null"],"description":"The new title, or null to clear it."}},"required":["panel"],"additionalProperties":false}"#;

const SHOW_DESCRIPTION: &str = r#"Show a Mermaid diagram next to this terminal: the quickest way to draw one. It is create_panel with renderer "mermaid" and lifetime "scratch": Mermaid 12.1.0 syntax, checked by Pele 1.13.0, at most 204,800 bytes of UTF-8, kept in the daemon's data (not the project) until 7 days after its last change. The call waits up to about 5 s and answers "validated", "failed" (code, message, line, column, snippet) or "validating"; fix a failed diagram with update_panel on the returned panel id. The idempotency_key and placement rules of create_panel apply."#;

const SHOW_SCHEMA: &str = r#"{"type":"object","properties":{"source":{"type":"string","description":"The Mermaid source (Mermaid 12.1.0 syntax): at most 204,800 bytes of UTF-8."},"title":{"type":"string","description":"The tab title: 1 to 1,024 bytes, no control characters."},"id":{"type":"string","description":"The panel ID: 1 to 255 bytes, no control characters, no /, not starting with a dot. Omit it and the daemon picks panel-<n>."},"idempotency_key":{"type":"string","description":"1 to 256 bytes. A repeated create with a key a live panel of this space has returns that panel."},"placement":{"type":"string","description":"The ID of the layout tile to show the panel in: a hint, stored with the panel and applied when a client or the daemon places it."}},"required":["source"],"additionalProperties":false}"#;

const STATUS_DESCRIPTION: &str = r#"Get a panel's current revision and state at once, without waiting: "validating", "validated", "validated_parser_only", "failed" (with code, message, line, column and snippet) or "removed". Give revision, a revision a call returned, to learn whether it is still current: an older one is answered with the current revision and superseded true. render_status lists what each connected Combine client last reported for the panel: the revision it drew, rendered or render_failed, and the error."#;

const STATUS_SCHEMA: &str = r#"{"type":"object","properties":{"panel":{"type":"string","description":"The panel's ID."},"revision":{"type":"integer","minimum":0,"description":"A revision a call returned; an older one is answered with the current revision and superseded true."}},"required":["panel"],"additionalProperties":false}"#;

const LIST_DESCRIPTION: &str = "List the panels of this terminal's space, sorted by id, each with its revision, state, last error, title, renderer, lifetime, placement hint and creator, and the folders under .workspace/panels that hold no usable panel (skipped, with reasons). count is the number of panels; if they don't all fit in one reply, the first ones by id are listed and truncated is true.";

const LIST_SCHEMA: &str = r#"{"type":"object","properties":{},"additionalProperties":false}"#;

const REMOVE_DESCRIPTION: &str = r#"Remove a panel. A persistent panel's .workspace/panels/<id>/ files are deleted; a scratch panel is deleted from the daemon's data. The result has state "removed". If the panel's files changed outside Combine since the daemon last read them, nothing is deleted and the call fails with remove_conflict, giving the panel's id, revision and state as they are now: look at the files or panel_status, then remove it again if you still want to. A removed id answers "removed" until the daemon restarts."#;

const REMOVE_SCHEMA: &str = r#"{"type":"object","properties":{"panel":{"type":"string","description":"The panel's ID."}},"required":["panel"],"additionalProperties":false}"#;

/// The six tools, in `TOOL_NAMES` order.
pub(super) const TOOLS: [Tool; 6] = [
    Tool {
        name: TOOL_NAMES[0],
        description: CREATE_DESCRIPTION,
        input_schema: CREATE_SCHEMA,
    },
    Tool {
        name: TOOL_NAMES[1],
        description: UPDATE_DESCRIPTION,
        input_schema: UPDATE_SCHEMA,
    },
    Tool {
        name: TOOL_NAMES[2],
        description: SHOW_DESCRIPTION,
        input_schema: SHOW_SCHEMA,
    },
    Tool {
        name: TOOL_NAMES[3],
        description: STATUS_DESCRIPTION,
        input_schema: STATUS_SCHEMA,
    },
    Tool {
        name: TOOL_NAMES[4],
        description: LIST_DESCRIPTION,
        input_schema: LIST_SCHEMA,
    },
    Tool {
        name: TOOL_NAMES[5],
        description: REMOVE_DESCRIPTION,
        input_schema: REMOVE_SCHEMA,
    },
];

/// The `tools/list` result: the six entries and nothing else.
pub(super) fn list_result() -> String {
    let entries: Vec<String> = TOOLS
        .iter()
        .map(|tool| {
            format!(
                r#"{{"name":{},"description":{},"inputSchema":{}}}"#,
                super::jsonrpc::quoted(tool.name),
                super::jsonrpc::quoted(tool.description),
                tool.input_schema
            )
        })
        .collect();
    format!(r#"{{"tools":[{}]}}"#, entries.join(","))
}
