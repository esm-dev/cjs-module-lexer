use crate::cjs::CJSLexer;
use crate::error::{DiagnosticBuffer, ErrorBuffer};

use indexmap::{IndexMap, IndexSet};
use std::path::Path;
use swc_common::{
  comments::SingleThreadedComments,
  errors::{Handler, HandlerFlags},
  FileName, SourceMap,
};
use swc_ecma_ast::EsVersion;
use swc_ecmascript::{
  ast::{Module, Program},
  parser::{lexer::Lexer, EsConfig, StringInput, Syntax},
  visit::FoldWith,
};

pub struct SWC {
  pub module: Module,
}

impl SWC {
  /// parse source code.
  pub fn parse(specifier: &str, source: &str) -> Result<Self, anyhow::Error> {
    let source_map = SourceMap::default();
    let source_file = source_map.new_source_file(FileName::Real(Path::new(specifier).to_path_buf()), source.into());
    let sm = &source_map;
    let error_buffer = ErrorBuffer::new(specifier);
    let syntax = Syntax::Es(EsConfig::default());
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
    let module = parser
      .parse_module()
      .map_err(move |err| {
        let mut diagnostic = err.into_diagnostic(&handler);
        diagnostic.emit();
        DiagnosticBuffer::from_error_buffer(error_buffer, |span| sm.lookup_char_pos(span.lo))
      })
      .unwrap();

    Ok(SWC { module })
  }

  /// get named exports and reexports of the module.
  pub fn parse_cjs_exports(
    &self,
    node_env: &str,
    call_mode: bool,
  ) -> Result<(Vec<String>, Vec<String>), anyhow::Error> {
    let mut lexer = CJSLexer {
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
    Ok((
      lexer.named_exports.into_iter().collect(),
      lexer.reexports.into_iter().collect(),
    ))
  }
}


