mod error;
mod lexer;
mod test;

use crate::error::{DiagnosticBuffer, ErrorBuffer};
use crate::lexer::ModuleLexer;

use indexmap::{IndexMap, IndexSet};
use std::path::Path;
use swc_common::comments::SingleThreadedComments;
use swc_common::errors::{Handler, HandlerFlags};
use swc_common::{FileName, SourceMap};
use swc_ecmascript::ast::{EsVersion, Module, Program};
use swc_ecmascript::parser::{lexer::Lexer, EsSyntax, StringInput, Syntax};
use swc_ecmascript::visit::FoldWith;

pub struct CommonJSModuleLexer {
  pub module: Module,
}

impl CommonJSModuleLexer {
  /// parse the module from the source code.
  pub fn init(specifier: &str, source: &str) -> Result<Self, DiagnosticBuffer> {
    let source_map = SourceMap::default();
    let source_file =
      source_map.new_source_file(FileName::Real(Path::new(specifier).to_path_buf()).into(), source.into());
    let sm = &source_map;
    let error_buffer = ErrorBuffer::new(specifier);
    let syntax = Syntax::Es(EsSyntax::default());
    let input = StringInput::from(&*source_file);
    let comments = SingleThreadedComments::default();
    let lexer = Lexer::new(syntax, EsVersion::Es2020, input, Some(&comments));
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
    let mut lexer = ModuleLexer {
      call_mode,
      node_env: node_env.to_owned(),
      fn_returned: false,
      idents: IndexMap::new(),
      exports_alias: IndexSet::new(),
      named_exports: IndexSet::new(),
      reexports: IndexSet::new(),
    };
    let program = Program::Module(self.module.clone());
    program.fold_with(&mut lexer);
    (
      lexer.named_exports.into_iter().collect(),
      lexer.reexports.into_iter().collect(),
    )
  }
}
