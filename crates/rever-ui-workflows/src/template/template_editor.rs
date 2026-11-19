//! Port of `src/renderer/src/workflows/template/TemplateEditor.tsx` — a prompt template's name and body.

use std::rc::Rc;

use gpui::{AnyView, App, Context, Entity, Window, div, prelude::*, px};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::TextInput;
use rever_ui_kit::theme::ThemeMode;
use serde_json::{Value, json};

use crate::core::registry::EditorProps;
use crate::core::types::Workflow;
use crate::text_area::TextArea;
use crate::ui::{Tokens, area_box, input_box, new_area, new_input, text};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemplateData {
    pub body: String,
}

impl TemplateData {
    /// The `data` of a template workflow; anything without a text body is empty.
    pub fn from_value(data: &Value) -> Self {
        let body = data.get("body").and_then(Value::as_str).unwrap_or_default();
        Self {
            body: body.to_owned(),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({ "body": self.body })
    }
}

pub struct TemplateEditor {
    workflow: Workflow,
    on_change: Rc<dyn Fn(Workflow, &mut App)>,
    name: Entity<TextInput>,
    body: Entity<TextArea>,
    theme_mode: ThemeMode,
}

impl TemplateEditor {
    pub fn new(props: EditorProps, cx: &mut Context<Self>) -> Self {
        let lang: Lang = props.lang;
        let data = TemplateData::from_value(&props.workflow.data);
        let name = new_input(&props.workflow.name, lang.t("template.goalPlaceholder"), cx);
        let body = new_area(&data.body, lang.t("template.bodyPlaceholder"), 8.0, cx);
        cx.observe(&name, |editor, name, cx| {
            let name = name.read(cx).text().to_owned();
            if editor.workflow.name != name {
                editor.workflow.name = name;
                editor.changed(cx);
            }
        })
        .detach();
        cx.observe(&body, |editor, body, cx| {
            let body = body.read(cx).text().to_owned();
            if TemplateData::from_value(&editor.workflow.data).body != body {
                editor.workflow.data = TemplateData { body }.to_value();
                editor.changed(cx);
            }
        })
        .detach();
        Self {
            workflow: props.workflow,
            on_change: props.on_change,
            name,
            body,
            theme_mode: props.theme_mode,
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        (self.on_change)(self.workflow.clone(), cx);
        cx.notify();
    }
}

/// The registry's editor factory.
pub fn open(props: EditorProps, _: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| TemplateEditor::new(props, cx)).into()
}

impl Render for TemplateEditor {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let label = || {
            text(12.0)
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(3.0))
                .text_color(t.theme.text_dim)
        };
        div()
            .id("template-editor")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(label().child("Name").child(
                input_box(&self.name, 28.0, 8.0, &t).debug_selector(|| "template-name".into()),
            ))
            .child(
                label().child("Prompt").child(
                    area_box(&self.body, 8, 12.0, 8.0, t.surface, &t)
                        .debug_selector(|| "template-body".into()),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 본문을_data에서_읽고_쓴다() {
        let data = TemplateData {
            body: "Explain {{request}}".into(),
        };
        assert_eq!(data.to_value(), json!({ "body": "Explain {{request}}" }));
        assert_eq!(TemplateData::from_value(&data.to_value()), data);
    }

    #[test]
    fn 본문이_글자가_아니면_빈_본문이다() {
        for data in [json!(null), json!({}), json!({ "body": 5 }), json!("text")] {
            assert_eq!(TemplateData::from_value(&data), TemplateData::default());
        }
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::RefCell;

    use gpui::{Modifiers, TestAppContext};

    use super::*;
    use crate::testing::{FakeHost, bind_keys};

    #[gpui::test]
    fn 이름과_본문을_고치면_바뀐_워크플로를_알린다(cx: &mut TestAppContext) {
        cx.update(bind_keys);
        let changes: Rc<RefCell<Vec<Workflow>>> = Rc::default();
        let props = EditorProps {
            workflow: Workflow {
                id: "w1".into(),
                kind: "template".into(),
                name: "Old".into(),
                data: json!({ "body": "Hi" }),
                ..Default::default()
            },
            on_change: Rc::new({
                let changes = changes.clone();
                move |workflow, _| changes.borrow_mut().push(workflow)
            }),
            host: Rc::new(FakeHost::default()),
            lang: Lang::En,
            theme_mode: ThemeMode::Light,
        };
        let (_, cx) = cx.add_window_view(move |_, cx| TemplateEditor::new(props, cx));
        // 열기만 해서는 바뀐 것이 없다.
        assert!(changes.borrow().is_empty());

        let name = cx.debug_bounds("template-name").unwrap();
        cx.simulate_click(name.center(), Modifiers::none());
        cx.simulate_input("er");
        cx.run_until_parked();
        assert_eq!(changes.borrow().last().unwrap().name, "Older");
        assert_eq!(
            changes.borrow().last().unwrap().data,
            json!({ "body": "Hi" })
        );

        let body = cx.debug_bounds("template-body").unwrap();
        cx.simulate_click(body.center(), Modifiers::none());
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("Line 1");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("Line 2");
        cx.run_until_parked();
        let last = changes.borrow().last().cloned().unwrap();
        assert_eq!(last.data, json!({ "body": "Line 1\nLine 2" }));
        assert_eq!(last.name, "Older");
        assert_eq!(last.id, "w1");
    }
}
