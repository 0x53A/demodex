use crate::{client::Client, model::{array,text}};
use demodex_protocol::Operation;
use serde_json::Value;
use std::rc::Rc;
use yew::prelude::*;

#[derive(Properties, Clone)]
pub struct Props {
    pub client: Option<Rc<Client>>,
    pub target: String,
    pub path: String,
    pub onchoose: Callback<String>,
}
impl PartialEq for Props {
    fn eq(&self, other:&Self)->bool {
        self.target==other.target && self.path==other.path && self.onchoose==other.onchoose && match (&self.client,&other.client) {
            (Some(a),Some(b))=>Rc::ptr_eq(a,b),(None,None)=>true,_=>false
        }
    }
}
pub enum Msg { None, Open, Close, Path(String), Browse(String), Loaded(u64,Result<Value,String>), Choose }
pub struct DirectoryPicker { open:bool, serial:u64, path:String, listing:Value, loading:bool, error:String }
impl Component for DirectoryPicker {
    type Message=Msg;
    type Properties=Props;
    fn create(_: &Context<Self>)->Self { Self { open:false,serial:0,path:String::new(),listing:Value::Null,loading:false,error:String::new() } }
    fn changed(&mut self,ctx:&Context<Self>,old:&Props)->bool {
        let same_client=match (&ctx.props().client,&old.client) {(Some(a),Some(b))=>Rc::ptr_eq(a,b),(None,None)=>true,_=>false};
        if !same_client || ctx.props().target!=old.target { self.open=false; self.serial+=1; }
        true
    }
    fn update(&mut self,ctx:&Context<Self>,msg:Msg)->bool {
        match msg {
            Msg::None=>return false,
            Msg::Open=>{self.open=true;ctx.link().send_message(Msg::Browse(if ctx.props().path.is_empty(){"/".into()}else{ctx.props().path.clone()}));}
            Msg::Close=>{self.open=false;self.serial+=1;}
            Msg::Path(path)=>self.path=path,
            Msg::Browse(path)=>{
                self.serial+=1;self.path=path.clone();self.loading=true;self.error.clear();self.listing=Value::Null;
                if let Some(client)=ctx.props().client.clone(){
                    let serial=self.serial;let target=ctx.props().target.clone();
                    ctx.link().send_future(async move { Msg::Loaded(serial,client.read(Operation::BrowseDirectories{target,path}).await.map_err(|e|format!("{e:#}"))) });
                }else{self.loading=false;self.error="Connect to browse directories".into();}
            }
            Msg::Loaded(serial,result)=>{
                if serial!=self.serial || !self.open {return false;}
                self.loading=false;
                match result {Ok(value)=>{self.path=text(&value,"path").into();self.listing=value;},Err(error)=>self.error=error}
            }
            Msg::Choose=>{
                if !self.loading && self.error.is_empty() && self.listing["path"].as_str()==Some(self.path.as_str()) {
                    ctx.props().onchoose.emit(self.path.clone());self.open=false;self.serial+=1;
                }
            }
        }
        true
    }
    fn view(&self,ctx:&Context<Self>)->Html {
        use wasm_bindgen::JsCast;
        let parent=self.listing["parent"].as_str().map(str::to_owned);
        html!{<><button type="button" class="browse-directory" disabled={ctx.props().client.is_none()} onclick={ctx.link().callback(|_|Msg::Open)}>{"Browse…"}</button>
        {if self.open{html!{<crate::modal::Modal title="Choose working directory" compact=true onclose={ctx.link().callback(|_|Msg::Close)}>
            <div class="directory-navigation"><input aria-label="Directory path" disabled={self.loading} value={self.path.clone()} oninput={ctx.link().callback(|e:InputEvent|Msg::Path(e.target().unwrap().unchecked_into::<web_sys::HtmlInputElement>().value()))} onkeydown={ctx.link().callback(move |e:KeyboardEvent|{if e.key()=="Enter"{e.prevent_default();Msg::Browse(e.target().unwrap().unchecked_into::<web_sys::HtmlInputElement>().value())}else{Msg::None}})}/>
            <button type="button" disabled={self.loading} onclick={ctx.link().callback({let path=self.path.clone();move |_|Msg::Browse(path.clone())})}>{"Go"}</button>
            <button type="button" disabled={parent.is_none()||self.loading} onclick={ctx.link().callback(move |_|Msg::Browse(parent.clone().unwrap_or_default()))}>{"Up"}</button></div>
            {if self.loading{html!{<p role="status">{"Loading directories…"}</p>}}else{Html::default()}}
            {if !self.error.is_empty(){html!{<p class="error" role="alert">{&self.error}</p>}}else{Html::default()}}
            <div class="directory-entries">{for array(&self.listing["entries"]).iter().map(|entry|{let path=text(entry,"path").to_owned();html!{<button type="button" onclick={ctx.link().callback(move |_|Msg::Browse(path.clone()))}><span aria-hidden="true">{"▱ "}</span>{text(entry,"name")}</button>}})}</div>
            {if !self.loading && self.listing["entries"].as_array().is_some_and(Vec::is_empty){html!{<p class="muted">{"No subdirectories"}</p>}}else{Html::default()}}
            {if self.listing["truncated"]==true{html!{<p class="muted">{"Listing shortened. Enter a path to reach another directory."}</p>}}else{Html::default()}}
            <button type="button" disabled={self.loading||!self.error.is_empty()||self.listing["path"].as_str()!=Some(self.path.as_str())} onclick={ctx.link().callback(|_|Msg::Choose)}>{"Use this directory"}</button>
        </crate::modal::Modal>}}else{Html::default()}}</>}
    }
}
