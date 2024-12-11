use lexer::CommonJSModuleLexer;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Options {
  node_env: Option<String>,
  call_mode: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
  pub exports: Vec<String>,
  pub reexports: Vec<String>,
}

#[wasm_bindgen(js_name = "parse")]
pub fn parse(filename: &str, code: &str, options: JsValue) -> Result<JsValue, JsValue> {
  let options: Options = serde_wasm_bindgen::from_value(options).unwrap_or(Options {
    node_env: None,
    call_mode: None,
  });
  let lexer = match CommonJSModuleLexer::init(filename, code) {
    Ok(lexer) => lexer,
    Err(e) => {
      return Err(JsError::new(&e.to_string()).into());
    }
  };
  let node_env = if let Some(env) = options.node_env {
    env
  } else {
    "production".to_owned()
  };
  let call_mode = if let Some(ok) = options.call_mode { ok } else { false };
  let (exports, reexports) = lexer.analyze(&node_env, call_mode);
  Ok(serde_wasm_bindgen::to_value(&Output { exports, reexports }).unwrap())
}
