use indexmap::IndexSet;
use lexer::CjsModuleLexer;
use oxc_resolver::{ResolveError, ResolveOptions, Resolver};
use std::io::{stdout, Write};
use std::path::Path;
use std::{env, fs};

fn main() {
  let mut stdout = stdout();
  let specifier = env::args().skip(1).next().expect("missing specifier argument");
  let node_env = env::var("NODE_ENV").unwrap_or("production".to_owned());
  let wd = env::current_dir()
    .expect("failed to get current working directory")
    .to_str()
    .unwrap()
    .to_owned();
  let js_filename = resolve(&wd, &specifier, None).expect("failed to resolve specifier");
  let mut requires = vec![(js_filename, false)];
  let mut named_exports = IndexSet::new();
  while requires.len() > 0 {
    let (js_filename, call_mode) = requires.pop().unwrap();
    let code = fs::read_to_string(&js_filename).expect(("failed to read ".to_owned() + js_filename.as_str()).as_str());
    let lexer = CjsModuleLexer::parse(&js_filename, &code).expect("failed to parse module");
    let (exports, reexports) = lexer.analyze(&node_env, call_mode);
    if exports.len() == 0 && reexports.len() == 1 && named_exports.len() == 0 {
      let reexport = reexports[0].clone();
      if !reexport.starts_with(".")
        && !reexport.starts_with("/")
        && !reexport.ends_with("()")
        && !is_node_builtin_module(&reexport)
      {
        stdout
          .write_all(("!".to_owned() + reexport.as_str()).as_bytes())
          .expect("failed to write result to stdout");
      }
    }
    for export in exports {
      named_exports.insert(export);
    }
    for reexport in reexports {
      let mut call_mode = false;
      let reexport = if reexport.ends_with("()") {
        call_mode = true;
        reexport[..reexport.len() - 2].to_owned()
      } else {
        reexport
      };
      if !is_node_builtin_module(&reexport) {
        requires.push((
          resolve(&wd, &reexport, Some(js_filename.clone())).expect("failed to resolve reexport"),
          call_mode,
        ));
      }
    }
  }
  for name in named_exports {
    stdout
      .write_all((name + "\n").as_bytes())
      .expect("failed to write result to stdout");
  }
}

fn resolve(wd: &str, specifier: &str, containing_filename: Option<String>) -> Result<String, ResolveError> {
  if specifier.starts_with("/") || specifier.starts_with("file://") {
    return Ok(specifier.to_owned());
  }
  let resolver = Resolver::new(ResolveOptions {
    condition_names: vec!["node".to_owned(), "require".to_owned()],
    ..Default::default()
  });
  if (specifier.starts_with("./") || specifier.starts_with("../")) && containing_filename.is_some() {
    let containing_filename = containing_filename.unwrap();
    let containing_dir = Path::new(&containing_filename).parent().unwrap();
    let ret = resolver.resolve(containing_dir, specifier)?;
    return Ok(ret.path().to_str().unwrap().to_owned());
  }
  if specifier.starts_with(".") {
    return Err(ResolveError::NotFound(specifier.to_owned()));
  }
  let ret = resolver.resolve(wd, specifier)?;
  Ok(ret.path().to_str().unwrap().to_owned())
}

fn is_node_builtin_module(specifier: &str) -> bool {
  match specifier {
    "_http_agent"
    | "_http_client"
    | "_http_common"
    | "_http_incoming"
    | "_http_outgoing"
    | "_http_server"
    | "_stream_duplex"
    | "_stream_passthrough"
    | "_stream_readable"
    | "_stream_transform"
    | "_stream_wrap"
    | "_stream_writable"
    | "_tls_common"
    | "_tls_wrap"
    | "assert"
    | "assert/strict"
    | "async_hooks"
    | "buffer"
    | "child_process"
    | "cluster"
    | "console"
    | "constants"
    | "crypto"
    | "dgram"
    | "diagnostics_channel"
    | "dns"
    | "dns/promises"
    | "domain"
    | "events"
    | "fs"
    | "fs/promises"
    | "http"
    | "http2"
    | "https"
    | "inspector"
    | "inspector/promises"
    | "module"
    | "net"
    | "os"
    | "path"
    | "path/posix"
    | "path/win32"
    | "perf_hooks"
    | "process"
    | "punycode"
    | "querystring"
    | "readline"
    | "readline/promises"
    | "repl"
    | "stream"
    | "stream/consumers"
    | "stream/promises"
    | "stream/web"
    | "string_decoder"
    | "sys"
    | "timers"
    | "timers/promises"
    | "tls"
    | "trace_events"
    | "tty"
    | "url"
    | "util"
    | "util/types"
    | "v8"
    | "vm"
    | "wasi"
    | "worker_threads"
    | "zlib" => true,
    _ => specifier.starts_with("node:"),
  }
}
