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
                        action: "save".into(),
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
            <h2>{"Session controls"}</h2><p class="muted">{"Use these controls directly, or type /model, /goal, /status, or /help to open them. Commands are not sent to the model."}</p>
            <form onsubmit={model_submit} class="model-controls"><h3>{"Model · /model"}</h3>
                <p class="accepted-model">{if effective.is_null(){"Accepted model: unconfirmed".into()}else{format!("{}: {} · {}{}",if props.state["connected"]==true{"Accepted model"}else{"Last known model"},text(effective,"model"),text(effective,"effort"),if text(effective,"serviceTier").is_empty(){String::new()}else{format!(" · {}",text(effective,"serviceTier"))})}}</p>
                {if !props.model_error.is_empty(){html!{<p role="status" class="error">{props.model_error.clone()}</p>}}else{Html::default()}}
                <label>{"Model"}<select ref={self.model_ref.clone()} aria-label="Model" disabled={model_disabled} onchange={change_select("model")}>
                    <option value="" selected={selected.is_empty()}>{"Select a model"}</option>
                    {if !selected.is_empty() && model.is_null(){html!{<option value={selected.clone()} selected=true>{format!("{} (not in catalog)",selected)}</option>}}else{Html::default()}}
                    {for models.iter().map(|m|html!{<option value={text(m,"model").to_owned()} selected={text(m,"model")==selected}>{text(m,"displayName")}</option>})}
                </select></label>
                <p class="muted">{text(&model,"description")}</p>
                <div class="control-fields"><label>{"Reasoning effort"}<select ref={self.effort_ref.clone()} aria-label="Reasoning effort" disabled={model_disabled} onchange={change_select("effort")}><option value="" selected={effort.is_empty()}>{"Select reasoning effort"}</option>{for array(&model["supportedReasoningEfforts"]).iter().map(|v|html!{<option value={text(v,"reasoningEffort").to_owned()} selected={text(v,"reasoningEffort")==effort}>{text(v,"reasoningEffort")}</option>})}</select></label>
                <label>{"Service tier"}<select ref={self.tier_ref.clone()} aria-label="Service tier" disabled={model_disabled} onchange={change_select("tier")}><option value="" selected={tier.is_empty()}>{"Default"}</option>{for array(&model["serviceTiers"]).iter().map(|v|html!{<option value={text(v,"id").to_owned()} selected={text(v,"id")==tier}>{text(v,"name")}</option>})}</select></label></div>
                <button disabled={model_disabled||selected.is_empty()||effort.is_empty()}>{"Apply model"}</button><button type="button" disabled={props.disabled} onclick={refresh}>{"Refresh models"}</button>
                {if props.working{html!{<p class="muted">{"Model changes are available when the session is idle."}</p>}}else{html!{<p class="muted">{"Selections take effect only after Apply model. The accepted model above is reported by Codex."}</p>}}}
            </form>
            <form onsubmit={goal_submit} class="goal-controls"><h3>{"Goal · /goal"}</h3>
                {if let Some(error)=props.state["goalError"].as_str(){html!{<p class="error" role="status">{error}</p>}}else if goal.is_null(){html!{<p class="goal-status">{"No goal set"}</p>}}else{html!{<div class="goal-state"><p class="goal-status">{format!("Goal: {}",text(goal,"status"))}</p><p>{text(goal,"objective")}</p><p class="muted">{format!("{} tokens used · {} seconds · {}",goal["tokensUsed"].as_i64().unwrap_or(0),goal["timeUsedSeconds"].as_i64().unwrap_or(0),goal["tokenBudget"].as_i64().map(|v|format!("{v} token budget")).unwrap_or_else(||"No token budget".into()))}</p></div>}}}
                <label>{"Goal objective"}<textarea disabled={goal_disabled||props.working} maxlength="4000" value={objective.clone()} oninput={objective_change}/></label>
                <label>{"Token budget (optional; blank keeps current budget)"}<input type="number" min="1" step="1" disabled={goal_disabled||props.working} value={budget} oninput={budget_change}/></label>
                <button disabled={goal_disabled||props.working||objective.trim().is_empty()}>{"Save goal paused"}</button>
                {if props.targets_pending{html!{<p class="muted">{"Send a message to apply the selected execution targets before resuming a goal."}</p>}}else{Html::default()}}
                <div class="goal-actions">
                    {action("pause","Pause goal",goal_disabled||goal.is_null()||text(goal,"status")!="active")}
                    {action("resume","Start / resume goal",goal_disabled||props.targets_pending||props.working||goal.is_null()||matches!(text(goal,"status"),"active"|"complete"))}
                    {action("complete","Mark goal complete",goal_disabled||props.working||goal.is_null()||text(goal,"status")=="complete")}
                    {action("clear","Clear goal",goal_disabled||props.working||goal.is_null())}
                </div>
                <p class="muted">{"Save stores the goal paused. Start / resume allows Codex to keep working and use tokens. Pause prevents further goal turns; use Interrupt to stop the current turn. Replacing the objective resets its usage accounting."}</p>
            </form>

        </section>}
    }
}
