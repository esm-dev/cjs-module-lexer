use indexmap::IndexSet;
use lexer::CommonJSModuleLexer;
use oxc_resolver::{ResolveError, ResolveOptions, Resolver};
use std::collections::HashSet;
use std::io::{self, stdout, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
  let specifier = env::args().skip(1).next().expect("missing specifier argument");
  let node_env = env::var("NODE_ENV").unwrap_or("production".to_owned());
  let wd = env::current_dir()
    .expect("failed to get current working directory")
    .to_str()
    .unwrap()
    .to_owned();
  let mut stdout = BufWriter::new(stdout().lock());
  for line in analyze(&wd, &specifier, &node_env) {
    stdout
      .write_all((line + "\n").as_bytes())
      .expect("failed to write result to stdout");
  }
  stdout.flush().expect("failed to write result to stdout");
}

/// analyze the module and its reexports, returns `["@<package>"]` if the module only reexports
/// another package, otherwise the named exports.
fn analyze(wd: &str, specifier: &str, node_env: &str) -> Vec<String> {
  let resolver = Resolver::new(ResolveOptions {
    condition_names: vec!["node".to_owned(), "require".to_owned()],
    ..Default::default()
  });
  let js_filename = if specifier.starts_with("/") {
    specifier.to_owned()
  } else if specifier.starts_with("./") || specifier.starts_with("../") {
    Path::join(Path::new(wd), Path::new(specifier))
      .to_str()
      .unwrap()
      .to_owned()
  } else {
    resolve(&resolver, wd, specifier, None).expect("failed to resolve specifier")
  };
  let mut requires = vec![(js_filename, false)];
  // reexports may be circular, analyze each module only once
  let mut analyzed = HashSet::new();
  let mut named_exports = IndexSet::new();
  while let Some((js_filename, call_mode)) = requires.pop() {
    let real_path = fs::canonicalize(&js_filename).unwrap_or_else(|_| PathBuf::from(&js_filename));
    if !analyzed.insert((real_path, call_mode)) {
      continue;
    }
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
    let (exports, reexports) = lexer.analyze(node_env, call_mode);
    // the module only reexports a package, unless other reexports are still pending
    if exports.len() == 0 && reexports.len() == 1 && named_exports.len() == 0 && requires.is_empty() {
      let reexport = reexports[0].clone();
      if !reexport.starts_with(".")
        && !reexport.starts_with("/")
        && !reexport.ends_with("()")
        && !is_node_builtin_module(&reexport)
      {
        return vec!["@".to_owned() + reexport.as_str()];
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
          resolve(&resolver, wd, &reexport, Some(&js_filename)).expect("failed to resolve reexport"),
          call_mode,
        ));
      }
    }
  }
  named_exports
    .into_iter()
    .filter(|name| is_js_identifier(name))
    .collect()
}

fn resolve(
  resolver: &Resolver,
  wd: &str,
  specifier: &str,
  containing_filename: Option<&str>,
) -> Result<String, ResolveError> {
  if specifier.starts_with("/") || specifier.starts_with("file://") {
    return Err(ResolveError::NotFound(specifier.to_owned()));
  }
  // bare specifiers are looked up from the dir of the containing file, like node does
  let base_dir = match containing_filename {
    Some(containing_filename) => Path::new(containing_filename).parent().unwrap(),
    None => Path::new(wd),
  };
  // `require('.')` and `require('..')` load the dir, using the `main` field of its package.json
  if (specifier.eq(".") || specifier.eq("..")) && containing_filename.is_some() {
    return Ok(
      resolver
        .resolve(base_dir, specifier)?
        .path()
        .to_str()
        .unwrap()
        .to_owned(),
    );
  }
  let mut specifier = specifier.to_owned();
  if specifier.starts_with("./") || specifier.starts_with("../") {
    if containing_filename.is_some() {
      specifier = base_dir.join(specifier).to_str().unwrap().to_owned();
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
    find_node_modules_dir(base_dir, &specifier)
      .unwrap_or_else(|| Path::new(wd).join("node_modules"))
      .join(&specifier)
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
  let ret = match resolver.resolve(base_dir, &specifier) {
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

/// find the closest `node_modules` dir from `base_dir` up that contains the package of the specifier.
fn find_node_modules_dir(base_dir: &Path, specifier: &str) -> Option<PathBuf> {
  let mut segments = specifier.split('/');
  let pkg_name = match segments.next()? {
    scope if scope.starts_with('@') => format!("{}/{}", scope, segments.next()?),
    name => name.to_owned(),
  };
  base_dir
    .ancestors()
    .map(|dir| dir.join("node_modules"))
    .find(|dir| dir.join(&pkg_name).is_dir())
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

#[cfg(test)]
mod tests {
  use super::*;

  /// create the files in a new temp dir and return the dir path.
  fn fixture(name: &str, files: &[(&str, &str)]) -> String {
    let dir = env::temp_dir().join(format!("cjs-module-lexer-test-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    for (path, content) in files {
      let path = dir.join(path);
      fs::create_dir_all(path.parent().unwrap()).unwrap();
      fs::write(path, content).unwrap();
    }
    dir.to_str().unwrap().to_owned()
  }

  #[test]
  fn circular_reexports() {
    let wd = fixture(
      "circular",
      &[
        ("a.js", "module.exports = { a: 1, ...require('./b') }"),
        ("b.js", "module.exports = { b: 1, ...require('./a') }"),
      ],
    );
    assert_eq!(analyze(&wd, "./a.js", "production").join(","), "a,b");
  }

  #[test]
  fn package_reexport() {
    let wd = fixture(
      "package-reexport",
      &[
        ("index.js", "module.exports = require('./lib')"),
        ("lib.js", "module.exports = require('pkg')"),
        ("node_modules/pkg/index.js", "exports.x = 1"),
      ],
    );
    assert_eq!(analyze(&wd, "./index.js", "production").join(","), "@pkg");
  }

  #[test]
  fn package_reexport_with_siblings() {
    let wd = fixture(
      "package-reexport-siblings",
      &[
        ("index.js", "module.exports = { ...require('./a'), ...require('./b') }"),
        ("a.js", "exports.fromA = 1"),
        ("b.js", "module.exports = require('pkg')"),
        ("node_modules/pkg/index.js", "exports.x = 1"),
      ],
    );
    assert_eq!(analyze(&wd, "./index.js", "production").join(","), "x,fromA");
  }

  #[test]
  fn nested_node_modules() {
    let wd = fixture(
      "nested-node-modules",
      &[
        (
          "node_modules/a/index.js",
          "module.exports = { fromA: 1, ...require('b') }",
        ),
        ("node_modules/a/node_modules/b/index.js", "exports.nestedB = 1"),
        ("node_modules/b/index.js", "exports.topB = 1"),
      ],
    );
    assert_eq!(analyze(&wd, "a", "production").join(","), "fromA,nestedB");
  }

  #[cfg(unix)]
  #[test]
  fn pnpm_node_modules() {
    let wd = fixture(
      "pnpm-node-modules",
      &[
        (
          "node_modules/.pnpm/a@1.0.0/node_modules/a/index.js",
          "module.exports = { fromA: 1, ...require('b') }",
        ),
        (
          "node_modules/.pnpm/b@1.0.0/node_modules/b/index.js",
          "exports.fromB = 1",
        ),
      ],
    );
    let node_modules = Path::new(&wd).join("node_modules");
    std::os::unix::fs::symlink(".pnpm/a@1.0.0/node_modules/a", node_modules.join("a")).unwrap();
    std::os::unix::fs::symlink(
      "../../b@1.0.0/node_modules/b",
      node_modules.join(".pnpm/a@1.0.0/node_modules/b"),
    )
    .unwrap();
    assert_eq!(analyze(&wd, "a", "production").join(","), "fromA,fromB");
  }

  #[test]
  fn require_dir() {
    let wd = fixture(
      "require-dir",
      &[
        ("package.json", r#"{ "main": "lib/main.js" }"#),
        ("entry.js", "module.exports = { own: 1, ...require('.') }"),
        ("lib/main.js", "exports.fromMain = 1"),
        ("lib/entry.js", "module.exports = { own: 1, ...require('..') }"),
      ],
    );
    assert_eq!(analyze(&wd, "./entry.js", "production").join(","), "own,fromMain");
    assert_eq!(analyze(&wd, "./lib/entry.js", "production").join(","), "own,fromMain");
  }
}
