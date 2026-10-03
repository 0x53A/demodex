use crate::model::{array, text};
use demodex_protocol::Operation;
use serde_json::Value;
use std::collections::BTreeMap;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub id: String,
    pub state: Value,
    pub models: Value,
    pub model_error: String,
    pub fields: BTreeMap<String, String>,
    pub disabled: bool,
    pub working: bool,
    pub targets_pending: bool,
    pub onfield: Callback<(String, String)>,
    pub onrun: Callback<Operation>,
    pub onrefresh: Callback<()>,
}

pub struct SessionControls {
    model_ref: NodeRef,
    effort_ref: NodeRef,
    tier_ref: NodeRef,
}
impl Component for SessionControls {
    type Message = ();
    type Properties = Props;
    fn create(_: &Context<Self>) -> Self {
        Self {
            model_ref: NodeRef::default(),
            effort_ref: NodeRef::default(),
            tier_ref: NodeRef::default(),
        }
    }
    fn rendered(&mut self, ctx: &Context<Self>, _: bool) {
        // Synchronize the DOM value after option lists change. Updating selected
        // attributes alone can leave a dirty native select on its first option.
        let props = ctx.props();
        let effective = &props.state["settings"]["effective"];
        for (reference, field, key) in [
            (&self.model_ref, "model", "model"),
            (&self.effort_ref, "effort", "effort"),
            (&self.tier_ref, "tier", "serviceTier"),
        ] {
            let fallback = text(effective, key);
            let fallback = if field == "tier" && fallback == "default" {
                ""
            } else {
                fallback
            };
            let value = props
                .fields
                .get(field)
                .map(String::as_str)
                .unwrap_or(fallback);
            if let Some(select) = reference.cast::<web_sys::HtmlSelectElement>() {
                if select.value() != value {
                    select.set_value(value);
                }
            }
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        let effective = &props.state["settings"]["effective"];
        let field = |name: &str, fallback: &str| {
            props
                .fields
                .get(name)
                .cloned()
                .unwrap_or_else(|| fallback.into())
        };
        let models = array(&props.models["data"]);
        let selected = field("model", text(effective, "model"));
        let model = models
            .iter()
            .find(|m| text(m, "model") == selected)
            .cloned()
            .unwrap_or_default();
        let effort = field("effort", text(effective, "effort"));
        let current_tier = text(effective, "serviceTier");
        let tier = field(
            "tier",
            if current_tier == "default" {
                ""
            } else {
                current_tier
            },
        );
        let goal = &props.state["goal"];
        let objective = field("objective", text(goal, "objective"));
        let budget = field("budget", "");
        let model_disabled = props.disabled || props.working || models.is_empty();
        let goal_disabled = props.disabled || props.state["goalError"].is_string();
        let change_select = |key: &'static str| {
            let callback = props.onfield.clone();
            Callback::from(move |e: Event| {
                callback.emit((
                    key.into(),
                    e.target_unchecked_into::<web_sys::HtmlSelectElement>()
                        .value(),
                ))
            })
        };
        let action = |action: &'static str, label: &str, disabled: bool| {
            let id = props.id.clone();
            let run = props.onrun.clone();
            html! {<button type="button" disabled={disabled} onclick={Callback::from(move |_|run.emit(Operation::Goal{id:id.clone(),input:demodex_protocol::GoalAction{action:action.into(),objective:None,token_budget:None}}))}>{label}</button>}
        };
        let model_submit = {
            let run = props.onrun.clone();
            let id = props.id.clone();
            let model = selected.clone();
            let effort = effort.clone();
            let tier = tier.clone();
            Callback::from(move |e: SubmitEvent| {
                e.prevent_default();
                run.emit(Operation::Model {
                    id: id.clone(),
                    input: demodex_protocol::ModelChoice {
                        model: model.clone(),
                        effort: effort.clone(),
                        service_tier: if tier.is_empty() {
                            None
                        } else {
                            Some(tier.clone())
                        },
                    },
                });
            })
        };
        let goal_submit = {
            let run = props.onrun.clone();
            let id = props.id.clone();
            let objective = objective.clone();
            let budget = budget.clone();
            Callback::from(move |e: SubmitEvent| {
                e.prevent_default();
                run.emit(Operation::Goal {
                    id: id.clone(),
                    input: demodex_protocol::GoalAction {
                        action: if e.submitter().is_some_and(|button|button.get_attribute("data-goal-action").as_deref()==Some("save")) { "save" } else { "start" }.into(),
                        objective: Some(objective.clone()),
                        token_budget: if budget.trim().is_empty() {
                            None
                        } else {
                            Some(budget.parse::<i64>().unwrap_or(0))
                        },
                    },
                });
            })
        };
        let objective_change = {
            let cb = props.onfield.clone();
            Callback::from(move |e: InputEvent| {
                cb.emit((
                    "objective".into(),
                    e.target_unchecked_into::<web_sys::HtmlTextAreaElement>()
                        .value(),
                ))
            })
        };
        let budget_change = {
            let cb = props.onfield.clone();
            Callback::from(move |e: InputEvent| {
                cb.emit((
                    "budget".into(),
                    e.target_unchecked_into::<web_sys::HtmlInputElement>()
                        .value(),
                ))
            })
        };
        let refresh = {
            let cb = props.onrefresh.clone();
            Callback::from(move |_| cb.emit(()))
        };
        html! {<section class="session-controls" aria-label="Session controls">

            <crate::ui::Form onsubmit={model_submit} class="model-controls" actions={html!{<><button disabled={model_disabled||selected.is_empty()||effort.is_empty()}>{"Apply model"}</button><button type="button" disabled={props.disabled} onclick={refresh}>{"Refresh models"}</button></>}}><crate::ui::Group title="Model" label="Model settings">
                <p class="accepted-model">{if effective.is_null(){"Accepted model: unconfirmed".into()}else{format!("{}: {} · {}{}",if props.state["connected"]==true{"Accepted model"}else{"Last known model"},text(effective,"model"),text(effective,"effort"),if text(effective,"serviceTier").is_empty(){String::new()}else{format!(" · {}",text(effective,"serviceTier"))})}}</p>
                {if !props.model_error.is_empty(){html!{<p role="status" class="error">{props.model_error.clone()}</p>}}else{Html::default()}}
                <label>{"Model"}<select ref={self.model_ref.clone()} aria-label="Model" disabled={model_disabled} onchange={change_select("model")}>
                    <option value="" selected={selected.is_empty()}>{"Select a model"}</option>
                    {if !selected.is_empty() && model.is_null(){html!{<option value={selected.clone()} selected=true>{format!("{} (not in catalog)",selected)}</option>}}else{Html::default()}}
                    {for models.iter().map(|m|html!{<option value={text(m,"model").to_owned()} selected={text(m,"model")==selected}>{text(m,"displayName")}</option>})}
                </select></label>
                <p class="muted model-description">{text(&model,"description")}</p>
                <div class="control-fields"><label>{"Reasoning effort"}<select ref={self.effort_ref.clone()} aria-label="Reasoning effort" disabled={model_disabled} onchange={change_select("effort")}><option value="" selected={effort.is_empty()}>{"Select reasoning effort"}</option>{for array(&model["supportedReasoningEfforts"]).iter().map(|v|html!{<option value={text(v,"reasoningEffort").to_owned()} selected={text(v,"reasoningEffort")==effort}>{text(v,"reasoningEffort")}</option>})}</select></label>
                <label>{"Service tier"}<select ref={self.tier_ref.clone()} aria-label="Service tier" disabled={model_disabled} onchange={change_select("tier")}><option value="" selected={tier.is_empty()}>{"Default"}</option>{for array(&model["serviceTiers"]).iter().map(|v|html!{<option value={text(v,"id").to_owned()} selected={text(v,"id")==tier}>{text(v,"name")}</option>})}</select></label></div>

                {if props.working{html!{<p class="muted control-warning" role="status">{"Session active — wait until idle to change the model."}</p>}}else{Html::default()}}
            </crate::ui::Group></crate::ui::Form>
            <crate::ui::Group title="System prompt">
                <p class="muted">{"Apply the latest server prompt settings by reconnecting this idle session. Pause goals and clear queued work first. Changing models also reconnects when prompt layers are active."}</p>
                {if props.state["prompts"]["pending"]==true{html!{<p role="status" class="control-warning">{"New server prompt settings are available."}</p>}}else{Html::default()}}
                {if props.state["prompts"]["legacy"]==true{html!{<p class="control-warning">{"This session has a legacy complete prompt override. Applying replaces it with the server prompt layers."}</p>}}else{Html::default()}}
                {if let Some(sources)=props.state["prompts"]["applied"]["instruction_sources"].as_array(){html!{<details><summary>{"Instruction files reported by Codex at connection"}</summary><ul>{for sources.iter().filter_map(Value::as_str).map(|path|html!{<li><code>{path}</code></li>})}</ul></details>}}else{Html::default()}}
                <label class="checkbox"><input type="checkbox" checked={field("include_project",if props.state["prompts"]["include_project"]==false{"false"}else{"true"})!="false"} onchange={props.onfield.reform(|e:Event|("include_project".into(),e.target_unchecked_into::<web_sys::HtmlInputElement>().checked().to_string()))}/>{"Include project instruction files"}</label>
                <button type="button" disabled={props.disabled||props.working} onclick={props.onrun.reform({let id=props.id.clone();let include=field("include_project",if props.state["prompts"]["include_project"]==false{"false"}else{"true"})!="false";move |_|Operation::ApplySessionPromptSettings{id:id.clone(),include_project:Some(include)}})}>{"Apply prompts and reconnect"}</button>
                <button type="button" disabled={props.disabled||props.working} onclick={props.onrun.reform({let id=props.id.clone();move |_|Operation::ApplySessionPromptSettings{id:id.clone(),include_project:None}})}>{"Use server inclusion default and reconnect"}</button>
            </crate::ui::Group>
            <crate::ui::Form onsubmit={goal_submit} class="goal-controls" actions={html!{<div class="goal-edit-actions">{if goal.is_null() || text(goal,"status")=="complete" || objective.trim()!=text(goal,"objective").trim() || !budget.is_empty() {html!{<><button class="primary" type="submit" disabled={goal_disabled||props.targets_pending||objective.trim().is_empty()}>{"Start goal"}</button><button type="submit" data-goal-action="save" disabled={goal_disabled||objective.trim().is_empty()}>{"Save paused"}</button></>}}else{Html::default()}}</div>}}><crate::ui::Group title="Goal">
                {if let Some(error)=props.state["goalError"].as_str(){html!{<p class="error" role="status">{error}</p>}}else if goal.is_null(){html!{<p class="goal-status">{"No goal set"}</p>}}else{html!{<div class="goal-state"><p class="goal-status">{format!("Goal: {}",text(goal,"status"))}</p><p>{text(goal,"objective")}</p><p class="muted">{format!("{} tokens used · {} seconds · {}",goal["tokensUsed"].as_i64().unwrap_or(0),goal["timeUsedSeconds"].as_i64().unwrap_or(0),goal["tokenBudget"].as_i64().map(|v|format!("{v} token budget")).unwrap_or_else(||"No token budget".into()))}</p></div>}}}
                <label>{"Goal objective"}<textarea disabled={goal_disabled} maxlength="4000" value={objective.clone()} oninput={objective_change}/></label>
                <label>{"Token budget (optional)"}<crate::ui::Input kind="number" min="1" step="1" placeholder="Keep current budget" disabled={goal_disabled} value={budget.clone()} oninput={budget_change} aria_label="Token budget (optional)" rule={crate::ui::Rule::Text}/></label>

                {if props.working{html!{<p class="muted control-warning" role="status">{"Session active — completing, clearing, or resuming an existing goal requires idle. You can set a new objective or pause the goal now."}</p>}}else{Html::default()}}
                {if props.targets_pending{html!{<p class="muted control-warning" role="status">{"Send a message to apply the selected execution targets before resuming a goal."}</p>}}else{Html::default()}}
                {if !goal.is_null(){html!{<div class="goal-actions">
                    {if text(goal,"status")=="active"{action("pause","Pause goal",goal_disabled)}else{Html::default()}}
                    {if !matches!(text(goal,"status"),"active"|"complete"){action("resume","Start / resume goal",goal_disabled||props.targets_pending||props.working)}else{Html::default()}}
                    {if !props.working && text(goal,"status")!="complete"{action("complete","Mark goal complete",goal_disabled)}else{Html::default()}}
                    {if !props.working{action("clear","Clear goal",goal_disabled)}else{Html::default()}}
                </div>}}else{Html::default()}}
                {if goal.is_object() && !objective.trim().is_empty() && objective.trim()!=text(goal,"objective").trim(){html!{<p class="muted control-warning" role="status">{"Replacing the objective resets goal usage accounting."}</p>}}else{Html::default()}}
            </crate::ui::Group></crate::ui::Form>

        </section>}
    }
}
