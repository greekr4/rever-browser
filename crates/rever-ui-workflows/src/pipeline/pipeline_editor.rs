//! Port of `src/renderer/src/workflows/pipeline/PipelineEditor.tsx` — the nested node view of a pipeline, and its runner.
//!
//! The React component edits the tree through nested controlled lists; here each list is
//! addressed by its path from the root ([`ListPath`]) and the edits are plain functions
//! over the tree.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, ClickEvent, Context, Entity, Rgba, Window, div, prelude::*, px,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::TextInput;
use rever_ui_kit::theme::ThemeMode;

use super::resolve::resolve_pipeline;
use super::types::{CondOn, CondOp, PipeCond, PipeNode, PipelineData, node_id};
use crate::core::registry::{EditorProps, OnChange};
use crate::core::types::Workflow;
use crate::host::{BranchTaken, McpToolInfo, PipeProgress, PipeStatus, WorkflowHost};
use crate::js::utf16_prefix;
use crate::text_area::TextArea;
use crate::ui::{
    ButtonKind, MONO, SANS, SelectOption, Tokens, area_box, button, field_label, html_text,
    input_box, new_area, new_input, select, text, tooltip,
};

// Inline English in the component (not dictionary keys), kept as it is there.
const LOADING_TOOLS: &str = "Loading tools…";
const SELECT_TOOL: &str = "Select a tool…";
const TOOL_PLACEHOLDER: &str = r#"{ "url": "https://..." }"#;
const VARS_LABEL: &str = "Variables (JSON, optional) — reference as {{name}} in tool inputs";
const VARS_PLACEHOLDER: &str = r#"{ "host": "example.com" }"#;
const RUN_PIPELINE: &str = "▶ Run pipeline";
const RUN_HINT: &str = "Runs on the active tab. Branches evaluate the previous tool's result.";
const EMPTY_PIPELINE: &str = "Pipeline is empty";

// ── The tree ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    Then,
    Else,
}

/// Where a node list is: from the root, the if-node to enter and which of its branches.
/// The root list is the empty path.
pub type ListPath = Vec<(usize, Branch)>;

fn list_mut<'a>(
    nodes: &'a mut Vec<PipeNode>,
    path: &[(usize, Branch)],
) -> Option<&'a mut Vec<PipeNode>> {
    let mut list = nodes;
    for (index, branch) in path {
        list = match (list.get_mut(*index)?, branch) {
            (PipeNode::If { then, .. }, Branch::Then) => then,
            (PipeNode::If { otherwise, .. }, Branch::Else) => otherwise,
            (PipeNode::Tool { .. }, _) => return None,
        };
    }
    Some(list)
}

/// Append `node` to the list at `path`. False when there is no such list.
pub fn add_node(nodes: &mut Vec<PipeNode>, path: &[(usize, Branch)], node: PipeNode) -> bool {
    list_mut(nodes, path).map(|list| list.push(node)).is_some()
}

pub fn remove_node(nodes: &mut Vec<PipeNode>, path: &[(usize, Branch)], index: usize) -> bool {
    match list_mut(nodes, path) {
        Some(list) if index < list.len() => {
            list.remove(index);
            true
        }
        _ => false,
    }
}

/// Swap the node at `index` with its neighbour above (-1) or below (+1) in the same list.
pub fn move_node(
    nodes: &mut Vec<PipeNode>,
    path: &[(usize, Branch)],
    index: usize,
    dir: isize,
) -> bool {
    let (Some(list), Some(other)) = (list_mut(nodes, path), index.checked_add_signed(dir)) else {
        return false;
    };
    if index >= list.len() || other >= list.len() {
        return false;
    }
    list.swap(index, other);
    true
}

/// The node with this id, anywhere in the tree.
fn find_mut<'a>(nodes: &'a mut [PipeNode], id: &str) -> Option<&'a mut PipeNode> {
    for node in nodes {
        if node.id() == id {
            return Some(node);
        }
        if let PipeNode::If {
            then, otherwise, ..
        } = node
            && let Some(found) = find_mut(then, id).or_else(|| find_mut(otherwise, id))
        {
            return Some(found);
        }
    }
    None
}

fn each_node<'a>(nodes: &'a [PipeNode], visit: &mut dyn FnMut(&'a PipeNode)) {
    for node in nodes {
        visit(node);
        if let PipeNode::If {
            then, otherwise, ..
        } = node
        {
            each_node(then, visit);
            each_node(otherwise, visit);
        }
    }
}

fn new_tool(tools: &[McpToolInfo]) -> PipeNode {
    PipeNode::Tool {
        id: node_id(),
        tool: tools
            .first()
            .map(|tool| tool.name.clone())
            .unwrap_or_default(),
        input: "{}".to_owned(),
    }
}

fn new_if() -> PipeNode {
    PipeNode::If {
        id: node_id(),
        cond: PipeCond::default(),
        then: Vec::new(),
        otherwise: Vec::new(),
    }
}

/// The line under a tool node while and after it runs.
pub fn progress_line(progress: &PipeProgress) -> String {
    match progress.status {
        PipeStatus::Running => "● running…".to_owned(),
        PipeStatus::Done => {
            let output = utf16_prefix(progress.output.as_deref().unwrap_or_default(), 200);
            format!("✓ {}", if output.is_empty() { "done" } else { output })
        }
        PipeStatus::Error => format!("✕ {}", progress.error.as_deref().unwrap_or("failed")),
        PipeStatus::Branch => String::new(),
    }
}

/// The line under an if node once it has chosen.
pub fn branch_line(progress: &PipeProgress) -> Option<String> {
    (progress.status == PipeStatus::Branch).then(|| {
        let taken = match progress.taken {
            Some(BranchTaken::Then) => "then",
            Some(BranchTaken::Else) => "else",
            None => "undefined",
        };
        format!("→ took “{taken}”")
    })
}

fn on_name(on: CondOn) -> &'static str {
    match on {
        CondOn::Output => "output",
        CondOn::Error => "error",
    }
}

fn op_name(op: CondOp) -> &'static str {
    match op {
        CondOp::Contains => "contains",
        CondOp::Equals => "equals",
        CondOp::Matches => "matches",
        CondOp::Always => "always",
    }
}

const ON_VALUES: [CondOn; 2] = [CondOn::Output, CondOn::Error];
const OP_VALUES: [CondOp; 4] = [
    CondOp::Contains,
    CondOp::Equals,
    CondOp::Matches,
    CondOp::Always,
];

/// A list's key in element ids: "root", or its path as "0t-2e".
fn path_key(path: &[(usize, Branch)]) -> String {
    if path.is_empty() {
        return "root".to_owned();
    }
    path.iter()
        .map(|(index, branch)| {
            format!(
                "{index}{}",
                match branch {
                    Branch::Then => 't',
                    Branch::Else => 'e',
                }
            )
        })
        .collect::<Vec<_>>()
        .join("-")
}

// ── The view ────────────────────────────────────────────────────────────────

/// The one text field of a node: a tool's input or a condition's value.
enum NodeField {
    Input(Entity<TextArea>),
    Value(Entity<TextInput>),
}

pub struct PipelineEditor {
    workflow: Workflow,
    data: PipelineData,
    on_change: OnChange,
    host: Rc<dyn WorkflowHost>,
    lang: Lang,
    theme_mode: ThemeMode,
    name: Entity<TextInput>,
    vars: Entity<TextArea>,
    /// By node id, so a field survives its node moving.
    fields: HashMap<String, NodeField>,
    tools: Vec<McpToolInfo>,
    running: bool,
    progress: HashMap<String, PipeProgress>,
    run_error: Option<String>,
    /// The select whose list is open: its element key.
    open_select: Option<String>,
}

impl PipelineEditor {
    pub fn new(props: EditorProps, cx: &mut Context<Self>) -> Self {
        let data = PipelineData::from_value(&props.workflow.data);
        let name = new_input(
            &props.workflow.name,
            props.lang.t("pipeline.goalPlaceholder"),
            cx,
        );
        let vars = new_area(&data.vars, VARS_PLACEHOLDER, 6.0, cx);
        cx.observe(&name, |editor, name, cx| {
            let name = name.read(cx).text().to_owned();
            if editor.workflow.name != name {
                editor.workflow.name = name;
                editor.changed(cx);
            }
        })
        .detach();
        cx.observe(&vars, |editor, vars, cx| {
            let vars = vars.read(cx).text().to_owned();
            if editor.data.vars != vars {
                editor.data.vars = vars;
                editor.commit(cx);
            }
        })
        .detach();

        let tools = props.host.list_tools(cx);
        cx.spawn(async move |editor, cx| {
            let tools = tools.await;
            let _ = editor.update(cx, |editor, cx| {
                editor.tools = tools;
                cx.notify();
            });
        })
        .detach();

        let mut editor = Self {
            workflow: props.workflow,
            data,
            on_change: props.on_change,
            host: props.host,
            lang: props.lang,
            theme_mode: props.theme_mode,
            name,
            vars,
            fields: HashMap::new(),
            tools: Vec::new(),
            running: false,
            progress: HashMap::new(),
            run_error: None,
            open_select: None,
        };
        editor.sync_fields(cx);
        editor
    }

    pub fn data(&self) -> &PipelineData {
        &self.data
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        (self.on_change)(self.workflow.clone(), cx);
        cx.notify();
    }

    /// The nodes or the variables changed: write them into the working copy.
    fn commit(&mut self, cx: &mut Context<Self>) {
        self.workflow.data = self.data.to_value();
        self.changed(cx);
    }

    fn update_node(
        &mut self,
        id: &str,
        patch: impl FnOnce(&mut PipeNode) -> bool,
        cx: &mut Context<Self>,
    ) {
        if find_mut(&mut self.data.nodes, id).is_some_and(patch) {
            self.commit(cx);
        }
    }

    fn update_cond(
        &mut self,
        id: &str,
        patch: impl FnOnce(&mut PipeCond) -> bool,
        cx: &mut Context<Self>,
    ) {
        self.update_node(
            id,
            |node| match node {
                PipeNode::If { cond, .. } => patch(cond),
                PipeNode::Tool { .. } => false,
            },
            cx,
        );
    }

    /// One field per node: made for a new node, dropped with a removed one.
    fn sync_fields(&mut self, cx: &mut Context<Self>) {
        let lang = self.lang;
        let mut live = HashSet::new();
        let mut missing = Vec::new();
        each_node(&self.data.nodes, &mut |node| {
            live.insert(node.id().to_owned());
            if !self.fields.contains_key(node.id()) {
                missing.push(node.clone());
            }
        });
        self.fields.retain(|id, _| live.contains(id));
        for node in missing {
            let id = node.id().to_owned();
            let field = match &node {
                PipeNode::Tool { input, .. } => {
                    let area = new_area(input, TOOL_PLACEHOLDER, 6.0, cx);
                    let id = id.clone();
                    cx.observe(&area, move |editor, area, cx| {
                        let text = area.read(cx).text().to_owned();
                        editor.update_node(
                            &id,
                            |node| match node {
                                PipeNode::Tool { input, .. } if *input != text => {
                                    *input = text;
                                    true
                                }
                                _ => false,
                            },
                            cx,
                        );
                    })
                    .detach();
                    NodeField::Input(area)
                }
                PipeNode::If { cond, .. } => {
                    let value = new_input(&cond.value, lang.t("pipeline.valuePlaceholder"), cx);
                    let id = id.clone();
                    cx.observe(&value, move |editor, value, cx| {
                        let text = value.read(cx).text().to_owned();
                        editor.update_cond(
                            &id,
                            |cond| {
                                let changed = cond.value != text;
                                cond.value = text;
                                changed
                            },
                            cx,
                        );
                    })
                    .detach();
                    NodeField::Value(value)
                }
            };
            self.fields.insert(id, field);
        }
    }

    pub fn add(&mut self, path: &[(usize, Branch)], tool: bool, cx: &mut Context<Self>) {
        let node = if tool {
            new_tool(&self.tools)
        } else {
            new_if()
        };
        if add_node(&mut self.data.nodes, path, node) {
            self.sync_fields(cx);
            self.commit(cx);
        }
    }

    pub fn remove(&mut self, path: &[(usize, Branch)], index: usize, cx: &mut Context<Self>) {
        if remove_node(&mut self.data.nodes, path, index) {
            self.sync_fields(cx);
            self.commit(cx);
        }
    }

    pub fn move_by(
        &mut self,
        path: &[(usize, Branch)],
        index: usize,
        dir: isize,
        cx: &mut Context<Self>,
    ) {
        if move_node(&mut self.data.nodes, path, index, dir) {
            self.commit(cx);
        }
    }

    pub fn run(&mut self, cx: &mut Context<Self>) {
        let nodes = match resolve_pipeline(&self.data.nodes, &self.data.vars) {
            Ok(nodes) if nodes.is_empty() => Err(EMPTY_PIPELINE.to_owned()),
            other => other,
        };
        let nodes = match nodes {
            Ok(nodes) => nodes,
            Err(error) => {
                self.run_error = Some(error);
                cx.notify();
                return;
            }
        };
        self.run_error = None;
        self.progress.clear();
        self.running = true;
        cx.notify();
        let editor = cx.weak_entity();
        let run = self.host.run_pipeline(
            nodes,
            Box::new(move |progress, cx| {
                // Deferred: a host may report the first nodes before `run_pipeline`
                // returns, while this editor is still being updated.
                let editor = editor.clone();
                cx.defer(move |cx| {
                    let _ = editor.update(cx, |editor, cx| {
                        editor.progress.insert(progress.node_id.clone(), progress);
                        cx.notify();
                    });
                });
            }),
            cx,
        );
        cx.spawn(async move |editor, cx| {
            let outcome = run.await;
            let _ = editor.update(cx, |editor, cx| {
                if let Err(error) = outcome {
                    editor.run_error = Some(error);
                }
                editor.running = false;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.host.cancel(cx);
    }

    /// A `<select>` of this editor: `key` names it, `pick` gets the chosen value.
    #[expect(
        clippy::too_many_arguments,
        reason = "one call draws one select: the arguments are its props"
    )]
    fn select(
        &self,
        key: String,
        value: &str,
        options: Vec<SelectOption>,
        height: f32,
        t: &Tokens,
        cx: &mut Context<Self>,
        pick: impl Fn(&mut Self, &str, &mut Context<Self>) + 'static,
    ) -> gpui::Div {
        let open = self.open_select.as_deref() == Some(key.as_str());
        let toggle_key = key.clone();
        select(
            key,
            value,
            options,
            open,
            height,
            t,
            cx,
            move |editor: &mut Self, open, cx| {
                editor.open_select = open.then(|| toggle_key.clone());
                cx.notify();
            },
            pick,
        )
    }

    // `.control-label` of the component: dim, 11px.
    fn control_label(t: &Tokens) -> gpui::Div {
        text(11.0).text_color(t.theme.text_dim)
    }

    fn render_node(
        &self,
        path: &ListPath,
        index: usize,
        count: usize,
        node: &PipeNode,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let lang = self.lang;
        let id = node.id().to_owned();
        let progress = self.progress.get(&id);
        let border = match progress.map(|progress| progress.status) {
            Some(PipeStatus::Running) => t.theme.accent,
            Some(PipeStatus::Error) => t.error,
            _ => t.theme.border,
        };
        let (can_up, can_down) = (index > 0, index + 1 < count);
        let key = |what: &str| gpui::ElementId::Name(format!("pipe-{what}-{id}").into());

        let (up_path, down_path, remove_path) = (path.clone(), path.clone(), path.clone());
        let header = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                // text-transform: uppercase; GPUI has no letter-spacing (0.4px).
                Self::control_label(t).child(match node {
                    PipeNode::If { .. } => "IF",
                    PipeNode::Tool { .. } => "TOOL",
                }),
            )
            .child(
                div()
                    .ml_auto()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        button(key("up"), "↑", ButtonKind::Plain, can_up, t)
                            .tooltip(tooltip(lang.t("common.moveUp"), t))
                            .when(can_up, |up| {
                                up.on_click(cx.listener(move |editor, _: &ClickEvent, _, cx| {
                                    editor.move_by(&up_path, index, -1, cx)
                                }))
                            }),
                    )
                    .child(
                        button(key("down"), "↓", ButtonKind::Plain, can_down, t)
                            .tooltip(tooltip(lang.t("common.moveDown"), t))
                            .when(can_down, |down| {
                                down.on_click(cx.listener(move |editor, _: &ClickEvent, _, cx| {
                                    editor.move_by(&down_path, index, 1, cx)
                                }))
                            }),
                    )
                    .child(
                        button(key("remove"), "✕", ButtonKind::Plain, true, t)
                            .tooltip(tooltip(lang.t("common.remove"), t))
                            .on_click(cx.listener(move |editor, _: &ClickEvent, _, cx| {
                                editor.remove(&remove_path, index, cx)
                            })),
                    ),
            );

        let card = div()
            .id(key("node"))
            .debug_selector({
                let id = id.clone();
                move || format!("pipe-node-{id}")
            })
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.0))
            .p(px(8.0))
            .border_1()
            .border_color(border)
            .rounded(px(6.0))
            .bg(t.surface)
            .child(header);

        match node {
            PipeNode::Tool { tool, .. } => {
                let mut options = Vec::with_capacity(self.tools.len() + 1);
                if tool.is_empty() {
                    options.push(SelectOption::new("", SELECT_TOOL));
                }
                options.extend(
                    self.tools
                        .iter()
                        .map(|tool| SelectOption::plain(tool.name.clone())),
                );
                let pick_id = id.clone();
                let tool_select = self.select(
                    format!("pipe-tool-{id}"),
                    tool,
                    options,
                    26.0,
                    t,
                    cx,
                    move |editor, picked, cx| {
                        editor.update_node(
                            &pick_id,
                            |node| match node {
                                PipeNode::Tool { tool, .. } if tool != picked => {
                                    *tool = picked.to_owned();
                                    true
                                }
                                _ => false,
                            },
                            cx,
                        )
                    },
                );
                let input = match self.fields.get(&id) {
                    Some(NodeField::Input(area)) => {
                        let id = id.clone();
                        Some(
                            area_box(area, 2, 11.0, 6.0, t.theme.surface_2, t)
                                .debug_selector(move || format!("pipe-input-{id}")),
                        )
                    }
                    _ => None,
                };
                let line = progress.map(|progress| {
                    let color = match progress.status {
                        PipeStatus::Error => t.error,
                        PipeStatus::Running => t.theme.accent,
                        _ => t.chip_ok_text,
                    };
                    let id = id.clone();
                    text(11.0)
                        .debug_selector(move || format!("pipe-progress-{id}"))
                        .text_color(color)
                        .child(html_text(&progress_line(progress)))
                });
                card.child(tool_select)
                    .children(input)
                    .children(line)
                    .into_any_element()
            }
            PipeNode::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                let on_id = id.clone();
                let on_select = self.select(
                    format!("pipe-on-{id}"),
                    on_name(cond.on),
                    ON_VALUES
                        .iter()
                        .map(|on| SelectOption::plain(on_name(*on)))
                        .collect(),
                    24.0,
                    t,
                    cx,
                    move |editor, picked, cx| {
                        let Some(on) = ON_VALUES.into_iter().find(|on| on_name(*on) == picked)
                        else {
                            return;
                        };
                        editor.update_cond(
                            &on_id,
                            |cond| {
                                let changed = cond.on != on;
                                cond.on = on;
                                changed
                            },
                            cx,
                        )
                    },
                );
                let op_id = id.clone();
                let op_select = self.select(
                    format!("pipe-op-{id}"),
                    op_name(cond.op),
                    OP_VALUES
                        .iter()
                        .map(|op| SelectOption::plain(op_name(*op)))
                        .collect(),
                    24.0,
                    t,
                    cx,
                    move |editor, picked, cx| {
                        let Some(op) = OP_VALUES.into_iter().find(|op| op_name(*op) == picked)
                        else {
                            return;
                        };
                        editor.update_cond(
                            &op_id,
                            |cond| {
                                let changed = cond.op != op;
                                cond.op = op;
                                changed
                            },
                            cx,
                        )
                    },
                );
                let value = match self.fields.get(&id) {
                    Some(NodeField::Value(value)) if cond.op != CondOp::Always => {
                        let id = id.clone();
                        Some(
                            input_box(value, 24.0, 6.0, t)
                                .debug_selector(move || format!("pipe-value-{id}"))
                                .flex_1()
                                .min_w(px(80.0))
                                .font_family(MONO)
                                .text_size(px(11.0)),
                        )
                    }
                    _ => None,
                };
                let condition = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(4.0))
                    .child(Self::control_label(t).child(lang.t("pipeline.ifLast")))
                    // A native select is as wide as its longest option.
                    .child(on_select.w(px(78.0)).flex_none())
                    .child(op_select.w(px(92.0)).flex_none())
                    .children(value);
                let took = progress.and_then(branch_line).map(|line| {
                    let id = id.clone();
                    text(11.0)
                        .debug_selector(move || format!("pipe-branch-{id}"))
                        .text_color(t.accent_text)
                        .child(line)
                });

                let mut then_path = path.clone();
                then_path.push((index, Branch::Then));
                let mut else_path = path.clone();
                else_path.push((index, Branch::Else));
                card.child(condition)
                    .children(took)
                    .child(Self::branch(
                        "THEN",
                        t.chip_ok_border,
                        self.render_list(then, &then_path, t, cx),
                        t,
                    ))
                    .child(Self::branch(
                        "ELSE",
                        t.chip_warn_border,
                        self.render_list(otherwise, &else_path, t, cx),
                        t,
                    ))
                    .into_any_element()
            }
        }
    }

    fn branch(label: &'static str, color: Rgba, list: AnyElement, t: &Tokens) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .ml(px(2.0))
            .pl(px(8.0))
            .border_l_2()
            .border_color(color)
            .child(Self::control_label(t).child(label))
            .child(list)
    }

    /// A list of nodes followed by its "+ Tool" and "+ If" buttons. Each nested branch
    /// is just another list.
    fn render_list(
        &self,
        nodes: &[PipeNode],
        path: &ListPath,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let has_tools = !self.tools.is_empty();
        let key = path_key(path);
        let (tool_path, if_path) = (path.clone(), path.clone());
        let cards: Vec<AnyElement> = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| self.render_node(path, index, nodes.len(), node, t, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.0))
            .children(cards)
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        button(
                            gpui::ElementId::Name(format!("pipe-add-tool-{key}").into()),
                            "+ Tool",
                            ButtonKind::Plain,
                            has_tools,
                            t,
                        )
                        .when(has_tools, |add| {
                            add.on_click(cx.listener(move |editor, _: &ClickEvent, _, cx| {
                                editor.add(&tool_path, true, cx)
                            }))
                        }),
                    )
                    .child(
                        button(
                            gpui::ElementId::Name(format!("pipe-add-if-{key}").into()),
                            "+ If",
                            ButtonKind::Plain,
                            true,
                            t,
                        )
                        .on_click(cx.listener(
                            move |editor, _: &ClickEvent, _, cx| editor.add(&if_path, false, cx),
                        )),
                    ),
            )
            .into_any_element()
    }
}

/// The registry's editor factory.
pub fn open(props: EditorProps, _: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| PipelineEditor::new(props, cx)).into()
}

impl Render for PipelineEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let dim = t.theme.text_dim;
        let has_nodes = !self.data.nodes.is_empty();

        let flow = if self.tools.is_empty() {
            div()
                .flex_none()
                .text_color(dim)
                .italic()
                .child(LOADING_TOOLS)
                .into_any_element()
        } else {
            self.render_list(&self.data.nodes, &ListPath::new(), &t, cx)
        };

        let run_button = if self.running {
            button(
                "pipe-stop",
                self.lang.t("common.stop"),
                ButtonKind::Plain,
                true,
                &t,
            )
            .on_click(cx.listener(|editor, _: &ClickEvent, _, cx| editor.cancel(cx)))
        } else {
            button("pipe-run", RUN_PIPELINE, ButtonKind::Primary, has_nodes, &t)
                .when(has_nodes, |run| {
                    run.on_click(cx.listener(|editor, _: &ClickEvent, _, cx| editor.run(cx)))
                })
        };

        text(12.0)
            .id("pipeline-editor")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .font_family(SANS)
            .text_color(t.theme.text)
            .child(
                field_label(&t).child("Name").child(
                    input_box(&self.name, 28.0, 8.0, &t).debug_selector(|| "pipe-name".into()),
                ),
            )
            .child(flow)
            .child(
                field_label(&t).child(VARS_LABEL).child(
                    area_box(&self.vars, 2, 11.0, 6.0, t.theme.surface_2, &t)
                        .debug_selector(|| "pipe-vars".into()),
                ),
            )
            .when_some(self.run_error.clone(), |column, error| {
                column.child(
                    text(12.0)
                        .flex_none()
                        .debug_selector(|| "pipe-run-error".into())
                        .text_color(t.error)
                        .child(html_text(&error)),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.0))
                    .child(run_button)
                    .child(Self::control_label(&t).flex_1().min_w_0().child(RUN_HINT)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str) -> PipeNode {
        PipeNode::Tool {
            id: id.into(),
            tool: "t".into(),
            input: "{}".into(),
        }
    }

    fn branch(id: &str, then: Vec<PipeNode>, otherwise: Vec<PipeNode>) -> PipeNode {
        PipeNode::If {
            id: id.into(),
            cond: PipeCond::default(),
            then,
            otherwise,
        }
    }

    /// The tree as text: `a if(b|c)`.
    fn shape(nodes: &[PipeNode]) -> String {
        nodes
            .iter()
            .map(|node| match node {
                PipeNode::Tool { id, .. } => id.clone(),
                PipeNode::If {
                    id,
                    then,
                    otherwise,
                    ..
                } => format!("{id}({}|{})", shape(then), shape(otherwise)),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn tree() -> Vec<PipeNode> {
        vec![
            tool("a"),
            branch(
                "i",
                vec![tool("b"), tool("c")],
                vec![branch("j", vec![], vec![tool("d")])],
            ),
        ]
    }

    #[test]
    fn 목록_끝에_노드를_붙인다() {
        let mut nodes = tree();
        assert!(add_node(&mut nodes, &[], tool("z")));
        assert_eq!(shape(&nodes), "a i(b c|j(|d)) z");
        assert!(add_node(&mut nodes, &[(1, Branch::Then)], tool("y")));
        assert!(add_node(
            &mut nodes,
            &[(1, Branch::Else), (0, Branch::Then)],
            tool("x")
        ));
        assert_eq!(shape(&nodes), "a i(b c y|j(x|d)) z");
    }

    #[test]
    fn 없는_목록에는_붙이지_않는다() {
        let mut nodes = tree();
        // 도구 노드에는 가지가 없고, 범위를 벗어난 위치에도 없다.
        assert!(!add_node(&mut nodes, &[(0, Branch::Then)], tool("z")));
        assert!(!add_node(&mut nodes, &[(9, Branch::Then)], tool("z")));
        assert!(!add_node(
            &mut nodes,
            &[(1, Branch::Then), (0, Branch::Else)],
            tool("z")
        ));
        assert_eq!(shape(&nodes), "a i(b c|j(|d))");
    }

    #[test]
    fn 노드를_지우면_그_아래_가지도_사라진다() {
        let mut nodes = tree();
        assert!(remove_node(&mut nodes, &[(1, Branch::Then)], 0));
        assert_eq!(shape(&nodes), "a i(c|j(|d))");
        assert!(remove_node(&mut nodes, &[(1, Branch::Else)], 0));
        assert_eq!(shape(&nodes), "a i(c|)");
        assert!(!remove_node(&mut nodes, &[], 5));
        assert!(remove_node(&mut nodes, &[], 1));
        assert_eq!(shape(&nodes), "a");
    }

    #[test]
    fn 같은_목록_안에서_위아래로_옮긴다() {
        let mut nodes = tree();
        assert!(move_node(&mut nodes, &[], 0, 1));
        assert_eq!(shape(&nodes), "i(b c|j(|d)) a");
        assert!(move_node(&mut nodes, &[(0, Branch::Then)], 1, -1));
        assert_eq!(shape(&nodes), "i(c b|j(|d)) a");
    }

    #[test]
    fn 끝에서는_더_옮기지_않는다() {
        let mut nodes = tree();
        assert!(!move_node(&mut nodes, &[], 0, -1));
        assert!(!move_node(&mut nodes, &[], 1, 1));
        assert!(!move_node(&mut nodes, &[], 7, -1));
        assert!(!move_node(&mut nodes, &[(1, Branch::Else)], 0, 1));
        assert_eq!(shape(&nodes), shape(&tree()));
    }

    #[test]
    fn id로_깊은_곳의_노드를_찾는다() {
        let mut nodes = tree();
        assert_eq!(
            find_mut(&mut nodes, "d").map(|node| node.id().to_owned()),
            Some("d".into())
        );
        assert_eq!(
            find_mut(&mut nodes, "j").map(|node| node.id().to_owned()),
            Some("j".into())
        );
        assert!(find_mut(&mut nodes, "nope").is_none());
        let mut seen = Vec::new();
        each_node(&nodes, &mut |node| seen.push(node.id().to_owned()));
        assert_eq!(seen, ["a", "i", "b", "c", "j", "d"]);
    }

    #[test]
    fn 새_노드의_기본값() {
        let tools = [McpToolInfo {
            name: "browser_navigate".into(),
            ..Default::default()
        }];
        match new_tool(&tools) {
            PipeNode::Tool { id, tool, input } => {
                assert!(id.starts_with('n'));
                assert_eq!(tool, "browser_navigate");
                assert_eq!(input, "{}");
            }
            PipeNode::If { .. } => panic!("expected a tool node"),
        }
        assert!(matches!(new_tool(&[]), PipeNode::Tool { tool, .. } if tool.is_empty()));
        match new_if() {
            PipeNode::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                assert_eq!(
                    cond,
                    PipeCond {
                        on: CondOn::Output,
                        op: CondOp::Contains,
                        value: String::new()
                    }
                );
                assert!(then.is_empty() && otherwise.is_empty());
            }
            PipeNode::Tool { .. } => panic!("expected an if node"),
        }
    }

    fn progress(status: PipeStatus) -> PipeProgress {
        PipeProgress {
            node_id: "n1".into(),
            tool: None,
            status,
            output: None,
            error: None,
            taken: None,
        }
    }

    #[test]
    fn 진행_줄은_상태마다_다르다() {
        assert_eq!(progress_line(&progress(PipeStatus::Running)), "● running…");
        assert_eq!(progress_line(&progress(PipeStatus::Done)), "✓ done");
        assert_eq!(progress_line(&progress(PipeStatus::Error)), "✕ failed");
        assert_eq!(progress_line(&progress(PipeStatus::Branch)), "");
        let done = PipeProgress {
            output: Some("x".repeat(300)),
            ..progress(PipeStatus::Done)
        };
        // 출력은 200자까지만 보여준다.
        assert_eq!(progress_line(&done), format!("✓ {}", "x".repeat(200)));
        let failed = PipeProgress {
            error: Some("timeout".into()),
            ..progress(PipeStatus::Error)
        };
        assert_eq!(progress_line(&failed), "✕ timeout");
    }

    #[test]
    fn 고른_가지를_알린다() {
        let taken = PipeProgress {
            taken: Some(BranchTaken::Else),
            ..progress(PipeStatus::Branch)
        };
        assert_eq!(branch_line(&taken), Some("→ took “else”".to_owned()));
        let then = PipeProgress {
            taken: Some(BranchTaken::Then),
            ..progress(PipeStatus::Branch)
        };
        assert_eq!(branch_line(&then), Some("→ took “then”".to_owned()));
        assert_eq!(branch_line(&progress(PipeStatus::Done)), None);
    }

    #[test]
    fn 목록의_열쇠는_경로다() {
        assert_eq!(path_key(&[]), "root");
        assert_eq!(path_key(&[(0, Branch::Then), (2, Branch::Else)]), "0t-2e");
    }
}

#[cfg(test)]
mod view_tests {
    use gpui::{TestAppContext, VisualTestContext};
    use serde_json::{Value, json};

    use super::*;
    use crate::pipeline::resolve::ResolvedPipeNode;
    use crate::testing::{Changes, FakeHost, bind_keys, click, props, retype, shown, tool};

    fn host() -> FakeHost {
        FakeHost {
            tools: vec![tool("browser_navigate", ""), tool("browser_click", "")],
            ..Default::default()
        }
    }

    fn open<'a>(
        host: &Rc<FakeHost>,
        data: Value,
        cx: &'a mut TestAppContext,
    ) -> (Entity<PipelineEditor>, Changes, &'a mut VisualTestContext) {
        cx.update(bind_keys);
        let workflow = Workflow {
            id: "w1".into(),
            kind: "pipeline".into(),
            name: "Cart".into(),
            data,
            ..Default::default()
        };
        let (props, changes) = props(workflow, host.clone());
        let (editor, cx) = cx.add_window_view(move |_, cx| PipelineEditor::new(props, cx));
        (editor, changes, cx)
    }

    fn tool_node(id: &str) -> Value {
        json!({ "id": id, "type": "tool", "tool": "browser_navigate", "input": "{}" })
    }

    fn if_node(id: &str, then: Value, otherwise: Value) -> Value {
        json!({
            "id": id,
            "type": "if",
            "cond": { "on": "output", "op": "contains", "value": "" },
            "then": then,
            "else": otherwise
        })
    }

    /// The tree as text, ids only: `a i(b c|d)`.
    fn shape(nodes: &[PipeNode]) -> String {
        nodes
            .iter()
            .map(|node| match node {
                PipeNode::Tool { id, .. } => id.clone(),
                PipeNode::If {
                    id,
                    then,
                    otherwise,
                    ..
                } => format!("{id}({}|{})", shape(then), shape(otherwise)),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn tree(editor: &Entity<PipelineEditor>, cx: &mut VisualTestContext) -> String {
        editor.read_with(cx, |editor, _| shape(&editor.data.nodes))
    }

    #[gpui::test]
    fn 도구_목록이_오기_전에는_노드_목록을_보여주지_않는다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(FakeHost::default());
        let (_, _, cx) = open(&host, json!({ "nodes": [tool_node("n1")], "vars": "" }), cx);
        assert!(!shown(cx, "pipe-node-n1"));
        assert!(!shown(cx, "pipe-add-if-root"));
        assert!(shown(cx, "pipe-name") && shown(cx, "pipe-vars") && shown(cx, "pipe-run"));
    }

    #[gpui::test]
    fn 뿌리와_가지에_노드를_더한다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let (editor, changes, cx) = open(&host, json!({ "nodes": [], "vars": "" }), cx);
        click(cx, "pipe-add-tool-root");
        click(cx, "pipe-add-if-root");
        let nodes = editor.read_with(cx, |editor, _| editor.data.nodes.clone());
        assert_eq!(nodes.len(), 2);
        // 새 도구 노드는 첫 도구와 빈 입력, 새 조건은 "output contains"다.
        assert!(matches!(
            &nodes[0],
            PipeNode::Tool { id, tool, input }
                if id.starts_with('n') && tool == "browser_navigate" && input == "{}"
        ));
        assert!(matches!(
            &nodes[1],
            PipeNode::If { cond, then, otherwise, .. }
                if *cond == PipeCond::default() && then.is_empty() && otherwise.is_empty()
        ));

        // 조건 노드의 두 가지에도 같은 단추가 있다.
        click(cx, "pipe-add-tool-1t");
        click(cx, "pipe-add-if-1e");
        let nodes = editor.read_with(cx, |editor, _| editor.data.nodes.clone());
        let PipeNode::If {
            then, otherwise, ..
        } = &nodes[1]
        else {
            panic!("expected an if node");
        };
        assert!(matches!(then.as_slice(), [PipeNode::Tool { .. }]));
        assert!(matches!(otherwise.as_slice(), [PipeNode::If { .. }]));
        let saved = PipelineData::from_value(&changes.borrow().last().unwrap().data);
        assert_eq!(saved.nodes, nodes);
    }

    #[gpui::test]
    fn 노드를_제_목록_안에서_옮기고_지운다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let nodes = json!([
            tool_node("a"),
            if_node("i", json!([tool_node("b"), tool_node("c")]), json!([]))
        ]);
        let (editor, changes, cx) = open(&host, json!({ "nodes": nodes, "vars": "" }), cx);
        click(cx, "pipe-down-a");
        assert_eq!(tree(&editor, cx), "i(b c|) a");
        click(cx, "pipe-up-c");
        assert_eq!(tree(&editor, cx), "i(c b|) a");
        // 목록의 처음과 끝에서는 더 가지 않는다.
        let before = changes.borrow().len();
        click(cx, "pipe-up-c");
        click(cx, "pipe-down-b");
        click(cx, "pipe-down-a");
        assert_eq!(tree(&editor, cx), "i(c b|) a");
        assert_eq!(changes.borrow().len(), before);

        click(cx, "pipe-remove-c");
        assert_eq!(tree(&editor, cx), "i(b|) a");
        // 조건을 지우면 그 가지의 노드와 입력 칸도 사라진다.
        click(cx, "pipe-remove-i");
        assert_eq!(tree(&editor, cx), "a");
        let fields: Vec<String> =
            editor.read_with(cx, |editor, _| editor.fields.keys().cloned().collect());
        assert_eq!(fields, ["a"]);
        assert_eq!(
            changes.borrow().last().unwrap().data,
            json!({ "nodes": [tool_node("a")], "vars": "" })
        );
    }

    #[gpui::test]
    fn 도구와_입력과_조건과_변수를_고친다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let nodes = json!([tool_node("n1"), if_node("n2", json!([]), json!([]))]);
        let (_, changes, cx) = open(&host, json!({ "nodes": nodes, "vars": "" }), cx);
        retype(cx, "pipe-name", "Checkout");
        click(cx, "pipe-tool-n1");
        click(cx, "pipe-tool-n1-option-1");
        retype(cx, "pipe-input-n1", r#"{ "ref": "r2" }"#);
        click(cx, "pipe-on-n2");
        click(cx, "pipe-on-n2-option-1");
        click(cx, "pipe-op-n2");
        click(cx, "pipe-op-n2-option-2");
        retype(cx, "pipe-value-n2", "5\\d\\d");
        retype(cx, "pipe-vars", r#"{ "host": "a.test" }"#);
        let last = changes.borrow().last().cloned().unwrap();
        assert_eq!(last.name, "Checkout");
        assert_eq!(
            last.data,
            json!({
                "nodes": [
                    { "type": "tool", "id": "n1", "tool": "browser_click", "input": "{ \"ref\": \"r2\" }" },
                    {
                        "type": "if",
                        "id": "n2",
                        "cond": { "on": "error", "op": "matches", "value": "5\\d\\d" },
                        "then": [],
                        "else": []
                    }
                ],
                "vars": "{ \"host\": \"a.test\" }"
            })
        );

        // "always"로 바꿔도 적어 둔 값은 남는다.
        click(cx, "pipe-op-n2");
        click(cx, "pipe-op-n2-option-3");
        assert_eq!(
            changes.borrow().last().unwrap().data["nodes"][1]["cond"],
            json!({ "on": "error", "op": "always", "value": "5\\d\\d" })
        );
    }

    #[gpui::test]
    fn always_조건의_노드는_값_칸을_그리지_않는다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let always = json!({
            "id": "n1",
            "type": "if",
            "cond": { "on": "output", "op": "always", "value": "kept" },
            "then": [],
            "else": []
        });
        let nodes = json!([always, if_node("n2", json!([]), json!([]))]);
        let (editor, _, cx) = open(&host, json!({ "nodes": nodes, "vars": "" }), cx);
        cx.run_until_parked();
        // A selector that was never painted has no bounds: n1 opened as "always".
        assert!(shown(cx, "pipe-node-n1") && shown(cx, "pipe-op-n1"));
        assert!(!shown(cx, "pipe-value-n1"));
        assert!(shown(cx, "pipe-value-n2"));
        // 값은 가려질 뿐 지워지지 않는다.
        assert_eq!(
            editor.read_with(cx, |editor, _| editor.data.to_value())["nodes"][0]["cond"]["value"],
            "kept"
        );
    }

    #[gpui::test]
    fn 실행하면_풀린_나무를_보내고_노드마다_진행을_보여준다(
        cx: &mut TestAppContext,
    ) {
        let host = Rc::new(host());
        let progress = |id: &str, status, taken| PipeProgress {
            node_id: id.into(),
            tool: None,
            status,
            output: None,
            error: None,
            taken,
        };
        *host.pipe_results.borrow_mut() = vec![
            progress("n1", PipeStatus::Running, None),
            progress("n1", PipeStatus::Done, None),
            progress("n2", PipeStatus::Branch, Some(BranchTaken::Else)),
        ];
        let nodes = json!([
            { "id": "n1", "type": "tool", "tool": "browser_navigate", "input": "{ \"url\": \"https://{{host}}/\" }" },
            if_node("n2", json!([]), json!([tool_node("n3")]))
        ]);
        let (editor, changes, cx) = open(
            &host,
            json!({ "nodes": nodes, "vars": "{ \"host\": \"a.test\" }" }),
            cx,
        );
        click(cx, "pipe-run");
        assert_eq!(
            *host.pipeline_runs.borrow(),
            [vec![
                ResolvedPipeNode::Tool {
                    id: "n1".into(),
                    tool: "browser_navigate".into(),
                    input: json!({ "url": "https://a.test/" }),
                },
                ResolvedPipeNode::If {
                    id: "n2".into(),
                    cond: PipeCond::default(),
                    then: vec![],
                    otherwise: vec![ResolvedPipeNode::Tool {
                        id: "n3".into(),
                        tool: "browser_navigate".into(),
                        input: json!({}),
                    }],
                },
            ]]
        );
        let seen = editor.read_with(cx, |editor, _| {
            (
                editor.progress.get("n1").map(progress_line),
                editor.progress.get("n2").and_then(branch_line),
                editor.progress.contains_key("n3"),
                editor.running,
                editor.run_error.clone(),
            )
        });
        assert_eq!(
            seen,
            (
                Some("✓ done".to_owned()),
                Some("→ took “else”".to_owned()),
                false,
                false,
                None
            )
        );
        assert!(shown(cx, "pipe-progress-n1") && shown(cx, "pipe-branch-n2"));
        assert!(changes.borrow().is_empty());
    }

    #[gpui::test]
    fn 풀_수_없거나_빈_파이프라인은_실행하지_않는다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        let (editor, _, cx) = open(&host, json!({ "nodes": [], "vars": "" }), cx);
        let error = |cx: &mut VisualTestContext| editor.read_with(cx, |e, _| e.run_error.clone());
        // 노드가 없으면 실행 단추가 꺼져 있다.
        click(cx, "pipe-run");
        assert_eq!(error(cx), None);
        editor.update(cx, |editor, cx| editor.run(cx));
        assert_eq!(error(cx).as_deref(), Some("Pipeline is empty"));

        click(cx, "pipe-add-tool-root");
        retype(cx, "pipe-vars", "{ nope");
        click(cx, "pipe-run");
        assert_eq!(error(cx).as_deref(), Some("Variables are not valid JSON"));
        assert!(shown(cx, "pipe-run-error"));
        assert!(host.pipeline_runs.borrow().is_empty());

        retype(cx, "pipe-vars", "");
        *host.run_error.borrow_mut() = Some("executor offline".into());
        click(cx, "pipe-run");
        assert_eq!(error(cx).as_deref(), Some("executor offline"));
        assert_eq!(host.pipeline_runs.borrow().len(), 1);
        assert!(!editor.read_with(cx, |e, _| e.running));
    }

    #[gpui::test]
    fn 실행_중에는_멈춤_단추가_실행을_취소한다(cx: &mut TestAppContext) {
        let host = Rc::new(host());
        host.hold.set(true);
        let (editor, _, cx) = open(&host, json!({ "nodes": [tool_node("n1")], "vars": "" }), cx);
        click(cx, "pipe-run");
        assert!(editor.read_with(cx, |e, _| e.running));
        click(cx, "pipe-stop");
        assert_eq!(*host.cancels.borrow(), 1);
        assert_eq!(host.pipeline_runs.borrow().len(), 1);
    }
}
