use crate::{client::Client, model::{array,text}};
use demodex_protocol::Operation;
use serde_json::Value;
use std::{collections::BTreeSet,rc::Rc};
use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Properties)]
pub struct Props {
    pub client: Rc<Client>,
    #[prop_or_default] pub session: Option<String>,
    #[prop_or_default] pub disabled: bool,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.client,&other.client) && self.session==other.session && self.disabled==other.disabled
    }
}
pub struct PluginSettings {
    view: Value,
    selected: BTreeSet<String>,
    busy: bool,
    error: Option<String>,
}
pub enum Msg {
    Load, Toggle(String,bool), Defaults, Save,
    Loaded(Result<Value,String>), Saved(Result<Value,String>),
}
fn ids(value:&Value)->BTreeSet<String> { value.as_array().into_iter().flatten().filter_map(|v|v.as_str().map(str::to_owned)).collect() }
impl Component for PluginSettings {
    type Message=Msg;
    type Properties=Props;
    fn create(_: &Context<Self>)->Self { Self {view:Value::Null,selected:BTreeSet::new(),busy:false,error:None} }
    fn update(&mut self,ctx:&Context<Self>,msg:Msg)->bool {
        match msg {
            Msg::Loaded(result)=>{
                self.busy=false;
                match result {Ok(view)=>{self.selected=ids(&view["disabled"]);self.view=view;},Err(e)=>self.error=Some(e)}
            },
            Msg::Saved(result)=>{
                self.busy=false;
                match result {Ok(_)=>ctx.link().send_message(Msg::Load),Err(e)=>self.error=Some(e)}
            },
            Msg::Toggle(id,enabled)=>{if self.busy{return false;} if enabled {self.selected.remove(&id);}else{self.selected.insert(id);}},
            Msg::Defaults=>{if self.busy{return false;} self.selected=ids(&self.view["defaults"]);},
            Msg::Load|Msg::Save=>{
                if self.busy{return false;}
                let save=matches!(msg,Msg::Save);
                if save && (ctx.props().disabled || !self.view["disabled"].is_array()) {return false;}
                let op=if save {Operation::SavePluginSettings {
                    id:ctx.props().session.clone(),expected_revision:self.view["revision"].as_u64().unwrap_or(0),
                    expected_disabled:ids(&self.view["disabled"]).into_iter().collect(),disabled:self.selected.iter().cloned().collect(),
                }}else{Operation::PluginSettings{id:ctx.props().session.clone()}};
                self.busy=true;self.error=None;
                let client=ctx.props().client.clone();
                ctx.link().send_future(async move {
                    let result=if save{client.call(op,uuid::Uuid::new_v4().to_string()).await}else{client.read(op).await}.map_err(|e|format!("{e:#}"));
                    if save {Msg::Saved(result)}else{Msg::Loaded(result)}
                });
            }
        }
        true
    }
    fn view(&self,ctx:&Context<Self>)->Html {
        let session=ctx.props().session.is_some();
        let unavailable=!self.view["disabled"].is_array();
        let blocked=self.busy||ctx.props().disabled||unavailable||(session&&self.view["connected"]!=true);
        let rows=array(&self.view["plugins"]);
        let missing=self.selected.iter().filter(|id|!rows.iter().any(|p|p["id"].as_str()==Some(id.as_str()))).cloned().collect::<Vec<_>>();
        html!{<crate::ui::Group title={if session{"Session plugins"}else{"Default plugins"}} class="plugin-settings">
            <p class="muted">{if session{"Changes apply to subsequent turns. Server defaults do not change this session."}else{"Used by new Codex and Claude sessions. Existing sessions keep their selection."}}</p>
            <button type="button" disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Load)}>{if self.view.is_null(){"Load plugins"}else{"Refresh plugins"}}</button>
            {if !self.view.is_null(){html!{<>
                {for rows.iter().map(|p|{
                    let id=text(p,"id").to_owned();let globally_enabled=p["enabled"]==true;
                    html!{<label class="plugin-choice"><input type="checkbox" checked={globally_enabled&&!self.selected.contains(&id)} disabled={blocked||!globally_enabled} onchange={ctx.link().callback(move |e:Event|Msg::Toggle(id.clone(),e.target_unchecked_into::<HtmlInputElement>().checked()))}/><span>{text(p,"name")}{if globally_enabled{""}else{" (disabled in Codex)"}}<small>{text(p,"id")}</small></span></label>}
                })}
                {for missing.iter().map(|id|{let key=id.clone();html!{<label class="plugin-choice"><input type="checkbox" checked=false disabled={blocked} onchange={ctx.link().callback(move |e:Event|Msg::Toggle(key.clone(),e.target_unchecked_into::<HtmlInputElement>().checked()))}/><span>{id}<small>{"Unavailable; exclusion retained"}</small></span></label>}})}
                {if rows.is_empty(){html!{<p>{"No installed plugins reported."}</p>}}else{Html::default()}}
                {if unavailable{html!{<p>{"Connect this session and refresh to read its selection."}</p>}}else{Html::default()}}
                {if session{html!{<button type="button" disabled={blocked} onclick={ctx.link().callback(|_|Msg::Defaults)}>{"Use current server defaults"}</button>}}else{Html::default()}}
                <button type="button" disabled={blocked||self.selected==ids(&self.view["disabled"])} onclick={ctx.link().callback(|_|Msg::Save)}>{if session{"Apply plugins"}else{"Save defaults"}}</button>
                {for array(&self.view["errors"]).iter().map(|error|html!{<p role="alert">{format!("Plugin discovery: {error}")}</p>})}
            </>}}else{Html::default()}}
            {if let Some(error)=&self.error{html!{<p role="alert">{error}</p>}}else{Html::default()}}
        </crate::ui::Group>}
    }
}
