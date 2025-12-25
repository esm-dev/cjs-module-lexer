use indexmap::IndexSet;
use lexer::CommonJSModuleLexer;
use oxc_resolver::{ResolveError, ResolveOptions, Resolver};
use std::io::{self, stdout, Write};
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
  let js_filename = if specifier.starts_with("./") || specifier.starts_with("../") || specifier.starts_with("/") {
    Path::join(Path::new(&wd), Path::new(&specifier))
      .to_str()
      .unwrap()
      .to_owned()
  } else {
    resolve(&wd, &specifier, None).expect("failed to resolve specifier")
  };
  let mut requires = vec![(js_filename, false)];
  let mut named_exports = IndexSet::new();
  while requires.len() > 0 {
    let (js_filename, call_mode) = requires.pop().unwrap();
    let code = fs::read_to_string(&js_filename).expect(("failed to read ".to_owned() + js_filename.as_str()).as_str());
    if js_filename.ends_with(".json") {
      let value: serde_json::Value = serde_json::from_str(&code).unwrap();
      if let Some(value) = value.as_object() {
        for key in value.keys() {
          if is_js_identifier(&key) {
            named_exports.insert(key.clone());
          }
        }
      }
      continue;
    }
    let lexer = CommonJSModuleLexer::init(&js_filename, &code).expect("failed to parse module");
    let (exports, reexports) = lexer.analyze(&node_env, call_mode);
    if exports.len() == 0 && reexports.len() == 1 && named_exports.len() == 0 {
      let reexport = reexports[0].clone();
      if !reexport.starts_with(".")
        && !reexport.starts_with("/")
        && !reexport.ends_with("()")
        && !is_node_builtin_module(&reexport)
      {
        stdout
          .write_all(("@".to_owned() + reexport.as_str() + "\n").as_bytes())
          .expect("failed to write result to stdout");
        return;
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
    if is_js_identifier(&name) {
      stdout
        .write_all((name + "\n").as_bytes())
        .expect("failed to write result to stdout");
    }
  }
}

fn resolve(wd: &str, specifier: &str, containing_filename: Option<String>) -> Result<String, ResolveError> {
  if specifier.starts_with("/") || specifier.starts_with("file://") {
    return Err(ResolveError::NotFound(specifier.to_owned()));
  }
  let mut specifier = specifier.to_owned();
  if specifier.eq(".") {
    specifier = "./index.js".to_owned();
  }
  if specifier.eq("..") {
    specifier = "../index.js".to_owned();
  }
  if specifier.starts_with("./") || specifier.starts_with("../") {
    if let Some(containing_filename) = containing_filename {
      let containing_dir = Path::new(&containing_filename).parent().unwrap();
      specifier = containing_dir.join(specifier).to_str().unwrap().to_owned();
    } else {
      return Err(ResolveError::NotFound(specifier.to_owned()));
    }
  }
  if specifier.starts_with(".") {
    return Err(ResolveError::NotFound(specifier.to_owned()));
  }

  let fullpath = if specifier.starts_with("/") {
    specifier.clone()
  } else {
    Path::join(Path::new(wd), "node_modules/".to_owned() + specifier.as_str())
      .to_str()
      .unwrap()
      .to_owned()
  };

  if (fullpath.ends_with(".js") || fullpath.ends_with(".cjs") || fullpath.ends_with(".json")) && file_exists(&fullpath)?
  {
    return Ok(fullpath);
  }
  if fullpath.ends_with(".js") {
    // path/to/file.js -> path/to/file.cjs
    let maybe_exists = fullpath[..fullpath.len() - 3].to_owned() + ".cjs";
    if file_exists(&maybe_exists)? {
      return Ok(maybe_exists);
    }
  }
  if fullpath.ends_with(".cjs") {
    // path/to/file.cjs -> path/to/file.js
    let maybe_exists = fullpath[..fullpath.len() - 4].to_owned() + ".js";
    if file_exists(&maybe_exists)? {
      return Ok(maybe_exists);
    }
  }

  // otherwise, let oxc_resolver do the job
  let resolver = Resolver::new(ResolveOptions {
    condition_names: vec!["node".to_owned(), "require".to_owned()],
    ..Default::default()
  });
  let ret = match resolver.resolve(wd, &specifier) {
    Ok(ret) => Ok(ret),
    Err(err) => match err {
      ResolveError::PackagePathNotExported { .. } => {
        // path/to/foo -> path/to/foo.cjs
        let maybe_exists = fullpath.to_owned() + ".cjs";
        if file_exists(&maybe_exists)? {
          return Ok(maybe_exists);
        }
        // path/to/foo -> path/to/foo.js
        let maybe_exists = fullpath.to_owned() + ".js";
        if file_exists(&maybe_exists)? {
          return Ok(maybe_exists);
        }
        Err(err)
      }
      _ => Err(err),
    },
  };
  Ok(ret?.path().to_str().unwrap().to_owned())
}

fn file_exists(path: &str) -> io::Result<bool> {
  match fs::metadata(path) {
    Ok(meta) => Ok(meta.is_file()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

fn is_js_identifier(s: &str) -> bool {
  if s.len() == 0 {
    return false;
  }
  let mut chars = s.chars();
  let first_char = chars.next().unwrap();
  if !is_alphabetic(first_char) {
    return false;
  }
  for c in chars {
    if !is_alphabetic(c) && !is_numberic(c) {
      return false;
    }
  }
  return true;
}

fn is_alphabetic(c: char) -> bool {
  match c {
    'a'..='z' | 'A'..='Z' | '_' | '$' => true,
    _ => false,
  }
}

fn is_numberic(c: char) -> bool {
  match c {
    '0'..='9' => true,
    _ => false,
  }
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
