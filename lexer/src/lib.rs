mod error;
mod lexer;
mod test;

use crate::error::{DiagnosticBuffer, ErrorBuffer};
use crate::lexer::ModuleLexer;

use bytes_str::BytesStr;
use std::path::Path;
use swc_common::errors::{Handler, HandlerFlags};
use swc_common::{FileName, SourceMap};
use swc_ecmascript::ast::{EsVersion, Module, ModuleItem};
use swc_ecmascript::parser::{lexer::Lexer, EsSyntax, StringInput, Syntax};

pub struct CommonJSModuleLexer {
  pub module: Module,
}

impl CommonJSModuleLexer {
  /// parse the module from the source code.
  pub fn init(specifier: &str, source: &str) -> Result<Self, DiagnosticBuffer> {
    let source_map = SourceMap::default();
    let source_file = source_map.new_source_file(
      FileName::Real(Path::new(specifier).to_path_buf()).into(),
      BytesStr::from_str_slice(source),
    );
    let sm = &source_map;
    let error_buffer = ErrorBuffer::new(specifier);
    let syntax = Syntax::Es(EsSyntax::default());
    let input = StringInput::from(&*source_file);
    // comments are not used, skip collecting them
    let lexer = Lexer::new(syntax, EsVersion::Es2020, input, None);
    let mut parser = swc_ecmascript::parser::Parser::new_from(lexer);
    let handler = Handler::with_emitter_and_flags(
      Box::new(error_buffer.clone()),
      HandlerFlags {
        can_emit_warnings: true,
        dont_buffer_diagnostics: true,
        ..HandlerFlags::default()
      },
    );
    let module = parser.parse_module().map_err(move |err| {
      let mut diagnostic = err.into_diagnostic(&handler);
      diagnostic.emit();
      DiagnosticBuffer::from_error_buffer(error_buffer, |span| sm.lookup_char_pos(span.lo))
    })?;
    Ok(CommonJSModuleLexer { module })
  }

  /// get named exports and reexports of the module.
  pub fn analyze(&self, node_env: &str, call_mode: bool) -> (Vec<String>, Vec<String>) {
    let mut lexer = ModuleLexer::new(node_env, call_mode);
    lexer.walk(
      self.module.body.iter().filter_map(|item| match item {
        ModuleItem::Stmt(stmt) => Some(stmt),
        _ => None,
      }),
      false,
    );
    (
      lexer.named_exports.into_iter().collect(),
      lexer.reexports.into_iter().collect(),
    )
  }
}
