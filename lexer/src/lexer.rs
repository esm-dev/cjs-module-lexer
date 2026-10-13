use indexmap::{IndexMap, IndexSet};
use std::rc::Rc;
use swc_common::DUMMY_SP;
use swc_ecmascript::ast::*;

/// A function body, either a block or a single expression (`() => expr`).
#[derive(Clone, Copy, Debug)]
pub enum FnBody<'a> {
  Block(&'a [Stmt]),
  Expr(&'a Expr),
}

#[derive(Clone, Debug)]
pub struct ObjDesc<'a> {
  props: &'a [PropOrSpread],
  // property names added later via `obj.foo = ...`
  extends: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct FnDesc<'a> {
  body: FnBody<'a>,
  extends: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Reexport {
  path: String,
  extends: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum IdentKind<'a> {
  Lit(Lit),
  Alias(String),
  Object(ObjDesc<'a>),
  Class(&'a Class),
  Fn(FnDesc<'a>),
  Reexport(Reexport),
  Unknown,
}

/// A change made inside a scope, reverted when the scope is left.
enum Undo<'a> {
  // the previous value of an ident, `None` if it was not declared
  Ident(Rc<str>, Option<IdentKind<'a>>),
  // a property name pushed to the `extends` of an ident
  Extend(Rc<str>),
  ExportsAlias(String),
}

pub struct ModuleLexer<'a> {
  pub node_env: String,
  pub call_mode: bool,
  pub fn_returned: bool,
  pub exports_alias: IndexSet<String>,
  pub idents: IndexMap<Rc<str>, IdentKind<'a>>,
  pub named_exports: IndexSet<String>,
  pub reexports: IndexSet<String>,
  // the bodies being walked, innermost last
  walking: Vec<(usize, usize)>,
  undo: Vec<Undo<'a>>,
}

impl<'a> ModuleLexer<'a> {
  pub fn new(node_env: &str, call_mode: bool) -> Self {
    ModuleLexer {
      node_env: node_env.to_owned(),
      call_mode,
      fn_returned: false,
      exports_alias: IndexSet::new(),
      idents: IndexMap::new(),
      named_exports: IndexSet::new(),
      reexports: IndexSet::new(),
      walking: Vec::new(),
      undo: Vec::new(),
    }
  }

  fn clear(&mut self) {
    self.named_exports.clear();
    self.reexports.clear();
  }

  fn set_ident(&mut self, name: &str, kind: IdentKind<'a>) {
    let name: Rc<str> = name.into();
    let prev = self.idents.insert(name.clone(), kind);
    // the top-level scope is never left, so it needs no undo
    if !self.walking.is_empty() {
      self.undo.push(Undo::Ident(name, prev));
    }
  }

  fn add_exports_alias(&mut self, name: &str) {
    if self.exports_alias.insert(name.to_owned()) && !self.walking.is_empty() {
      self.undo.push(Undo::ExportsAlias(name.to_owned()));
    }
  }

  // var foo = {}
  // foo.bar = 'bar'
  fn extend_ident(&mut self, name: &str, key: String) {
    // record the property on the resolved value, so aliases of it see the property too
    let Some((name, _)) = self.resolve_ident_entry(name) else {
      return;
    };
    let name = name.clone();
    match self.idents.get_mut(&name) {
      Some(
        IdentKind::Object(ObjDesc { extends, .. })
        | IdentKind::Fn(FnDesc { extends, .. })
        | IdentKind::Reexport(Reexport { extends, .. }),
      ) => extends.push(key),
      _ => return,
    }
    if !self.walking.is_empty() {
      self.undo.push(Undo::Extend(name));
    }
  }

  fn rollback(&mut self, len: usize) {
    while self.undo.len() > len {
      match self.undo.pop().unwrap() {
        Undo::Ident(name, Some(prev)) => {
          self.idents.insert(name, prev);
        }
        Undo::Ident(name, None) => {
          self.idents.swap_remove(&name);
        }
        Undo::Extend(name) => {
          if let Some(
            IdentKind::Object(ObjDesc { extends, .. })
            | IdentKind::Fn(FnDesc { extends, .. })
            | IdentKind::Reexport(Reexport { extends, .. }),
          ) = self.idents.get_mut(&name)
          {
            extends.pop();
          }
        }
        Undo::ExportsAlias(name) => {
          self.exports_alias.swap_remove(&name);
        }
      }
    }
  }

  /// resolve an ident to its value, following alias chains.
  fn resolve_ident(&self, name: &str) -> Option<&IdentKind<'a>> {
    self.resolve_ident_entry(name).map(|(_, kind)| kind)
  }

  /// like `resolve_ident`, but also returns the name of the ident holding the value.
  fn resolve_ident_entry(&self, name: &str) -> Option<(&Rc<str>, &IdentKind<'a>)> {
    let mut name = name;
    let mut hops = 0;
    loop {
      match self.idents.get_key_value(name)? {
        (_, IdentKind::Alias(next)) => {
          // an alias chain longer than the ident map must contain a cycle
          hops += 1;
          if hops > self.idents.len() {
            return None;
          }
          name = next;
        }
        entry => return Some(entry),
      }
    }
  }

  fn replace_exports_from_expr(&mut self, expr: &'a Expr) {
    if let Expr::Paren(ParenExpr { expr, .. }) = expr {
      self.replace_exports_from_expr(expr);
      return;
    }

    if let Some(reexport) = self.as_reexport(expr) {
      self.clear();
      for name in reexport.extends {
        self.named_exports.insert(name);
      }
      self.reexports.insert(reexport.path);
    } else if let Some(obj) = self.as_obj(expr) {
      self.clear();
      self.update_exports_from_object(&obj);
    } else if let Some(class) = self.as_class(expr) {
      self.clear();
      for name in get_class_static_names(class) {
        self.named_exports.insert(name);
      }
    } else if let Some(FnDesc { body, extends }) = self.as_function(expr) {
      self.clear();
      if self.call_mode {
        self.walk_body(body, true);
      } else {
        for name in extends {
          self.named_exports.insert(name);
        }
      }
    } else if let Expr::Call(call) = expr {
      if call.args.is_empty() {
        if let Some(callee) = with_expr_callee(call) {
          if let Some(reexport) = self.as_reexport(callee) {
            self.clear();
            for name in reexport.extends {
              self.named_exports.insert(name);
            }
            self.reexports.insert(format!("{}()", reexport.path));
          } else if let Some(FnDesc { body, .. }) = self.as_function(callee) {
            self.walk_body(body, true);
          }
        }
      }
    } else if let Expr::Assign(assign) = expr {
      self.replace_exports_from_expr(&assign.right);
    }
  }

  fn mark_ident(&mut self, name: &str, expr: &'a Expr) {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => self.mark_ident(name, expr),
      Expr::Lit(lit) => {
        self.set_ident(name, IdentKind::Lit(lit.clone()));
      }
      Expr::Ident(id) => {
        let alias = id.sym.as_ref();
        // `a = a` keeps the current value
        if alias == name {
          return;
        }
        let conflict = matches!(self.idents.get(alias), Some(IdentKind::Alias(rename)) if rename == name);
        if !conflict {
          self.set_ident(name, IdentKind::Alias(alias.into()));
        }
      }
      Expr::Call(call) => {
        if let Some(path) = with_require_call(call) {
          self.set_ident(name, IdentKind::Reexport(Reexport { path, extends: vec![] }));
        } else {
          self.set_ident(name, IdentKind::Unknown);
        }
      }
      Expr::Object(obj) => {
        self.set_ident(
          name,
          IdentKind::Object(ObjDesc {
            props: &obj.props,
            extends: vec![],
          }),
        );
      }
      Expr::Class(ClassExpr { class, .. }) => {
        self.set_ident(name, IdentKind::Class(class));
      }
      Expr::Arrow(arrow) => {
        self.set_ident(
          name,
          IdentKind::Fn(FnDesc {
            body: arrow_body(arrow),
            extends: vec![],
          }),
        );
      }
      Expr::Fn(FnExpr { function, .. }) => {
        if let Some(body) = &function.body {
          self.set_ident(
            name,
            IdentKind::Fn(FnDesc {
              body: FnBody::Block(&body.stmts),
              extends: vec![],
            }),
          );
        }
      }
      Expr::Member(_) => {
        if is_member_member(expr, "process", "env", "NODE_ENV") {
          self.set_ident(name, IdentKind::Lit(Lit::Str(quote_str(self.node_env.as_str()))));
        } else {
          self.set_ident(name, IdentKind::Unknown);
        }
      }
      _ => {
        self.set_ident(name, IdentKind::Unknown);
      }
    }
  }

  fn as_str(&self, expr: &Expr) -> Option<String> {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => self.as_str(expr),
      Expr::Lit(Lit::Str(Str { value, .. })) => Some(value.to_string_lossy().into_owned()),
      Expr::Ident(id) => match self.resolve_ident(id.sym.as_ref()) {
        Some(IdentKind::Lit(Lit::Str(Str { value, .. }))) => Some(value.to_string_lossy().into_owned()),
        _ => None,
      },
      Expr::Member(_) => {
        if is_member_member(expr, "process", "env", "NODE_ENV") {
          Some(self.node_env.to_owned())
        } else {
          None
        }
      }
      _ => None,
    }
  }

  fn as_num(&self, expr: &Expr) -> Option<f64> {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => self.as_num(expr),
      Expr::Lit(Lit::Num(Number { value, .. })) => Some(*value),
      Expr::Ident(id) => match self.resolve_ident(id.sym.as_ref()) {
        Some(IdentKind::Lit(Lit::Num(Number { value, .. }))) => Some(*value),
        _ => None,
      },
      _ => None,
    }
  }

  fn as_bool(&self, expr: &Expr) -> Option<bool> {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => self.as_bool(expr),
      Expr::Lit(Lit::Bool(Bool { value, .. })) => Some(*value),
      Expr::Ident(id) => match self.resolve_ident(id.sym.as_ref()) {
        Some(IdentKind::Lit(Lit::Bool(Bool { value, .. }))) => Some(*value),
        _ => None,
      },
      _ => None,
    }
  }

  fn as_null(&self, expr: &Expr) -> Option<bool> {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => self.as_null(expr),
      Expr::Lit(Lit::Null(_)) => Some(true),
      Expr::Ident(id) => match self.resolve_ident(id.sym.as_ref()) {
        Some(IdentKind::Lit(Lit::Null(_))) => Some(true),
        _ => None,
      },
      _ => None,
    }
  }

  fn as_obj(&self, expr: &'a Expr) -> Option<ObjDesc<'a>> {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => self.as_obj(expr),
      Expr::Object(ObjectLit { props, .. }) => Some(ObjDesc { props, extends: vec![] }),
      Expr::Ident(id) => match self.resolve_ident(id.sym.as_ref()) {
        Some(IdentKind::Object(obj)) => Some(obj.clone()),
        _ => None,
      },
      _ => None,
    }
  }

  fn as_reexport(&self, expr: &Expr) -> Option<Reexport> {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => self.as_reexport(expr),
      Expr::Call(call) => with_require_call(call).map(|path| Reexport { path, extends: vec![] }),
      Expr::Ident(id) => match self.resolve_ident(id.sym.as_ref()) {
        Some(IdentKind::Reexport(reexport)) => Some(reexport.clone()),
        _ => None,
      },
      _ => None,
    }
  }

  fn as_class(&self, expr: &'a Expr) -> Option<&'a Class> {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => self.as_class(expr),
      Expr::Class(ClassExpr { class, .. }) => Some(class),
      Expr::Ident(id) => match self.resolve_ident(id.sym.as_ref()) {
        Some(IdentKind::Class(class)) => Some(class),
        _ => None,
      },
      _ => None,
    }
  }

  fn as_function(&self, expr: &'a Expr) -> Option<FnDesc<'a>> {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => self.as_function(expr),
      Expr::Fn(FnExpr { function, .. }) => function.body.as_ref().map(|body| FnDesc {
        body: FnBody::Block(&body.stmts),
        extends: vec![],
      }),
      Expr::Ident(id) => match self.resolve_ident(id.sym.as_ref()) {
        Some(IdentKind::Fn(desc)) => Some(desc.clone()),
        _ => None,
      },
      _ => None,
    }
  }

  fn update_exports_from_object(&mut self, obj: &ObjDesc<'a>) {
    for prop in obj.props {
      match prop {
        PropOrSpread::Prop(prop) => {
          let name = match prop.as_ref() {
            Prop::Shorthand(id) => Some(id.sym.as_ref().to_owned()),
            Prop::KeyValue(KeyValueProp { key, .. }) => stringify_prop_name(key),
            Prop::Method(MethodProp { key, .. }) => stringify_prop_name(key),
            _ => None,
          };
          if let Some(name) = name {
            self.named_exports.insert(name);
          }
        }
        PropOrSpread::Spread(SpreadElement { expr, .. }) => match expr.as_ref() {
          Expr::Ident(_) => {
            if let Some(spread_obj) = self.as_obj(expr) {
              self.update_exports_from_object(&spread_obj);
            }
            if let Some(reexport) = self.as_reexport(expr) {
              self.reexports.insert(reexport.path);
            }
          }
          Expr::Call(call) => {
            if let Some(reexport) = with_require_call(call) {
              self.reexports.insert(reexport);
            }
          }
          _ => {}
        },
      }
    }
    for name in &obj.extends {
      self.named_exports.insert(name.clone());
    }
  }

  fn eqeq(&self, left: &Expr, right: &Expr) -> bool {
    if let Some(left) = self.as_str(left) {
      if let Some(right) = self.as_str(right) {
        return left == right;
      }
    } else if let Some(left) = self.as_num(left) {
      if let Some(right) = self.as_num(right) {
        return left == right;
      }
    } else if let Some(left) = self.as_bool(left) {
      if let Some(right) = self.as_bool(right) {
        return left == right;
      }
    } else if let Some(left) = self.as_null(left) {
      if let Some(right) = self.as_null(right) {
        return left == right;
      }
    }
    false
  }

  fn is_true(&self, expr: &Expr) -> bool {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => self.is_true(expr),
      Expr::Ident(id) => match self.resolve_ident(id.sym.as_ref()) {
        Some(IdentKind::Lit(lit)) => lit_is_true(lit),
        Some(_) => true,
        None => false, // undefined
      },
      Expr::Lit(lit) => lit_is_true(lit),
      Expr::Bin(BinExpr { op, left, right, .. }) => match op {
        BinaryOp::LogicalAnd => self.is_true(left) && self.is_true(right),
        BinaryOp::LogicalOr => self.is_true(left) || self.is_true(right),
        BinaryOp::EqEq | BinaryOp::EqEqEq => self.eqeq(left, right),
        BinaryOp::NotEq | BinaryOp::NotEqEq => !self.eqeq(left, right),
        _ => true,
      },
      _ => true,
    }
  }

  // var foo = module.exports = {};
  // var foo = module.exports;
  // var foo = exports;
  fn mark_exports_alias_from_var_decl(&mut self, decl: &VarDeclarator) {
    if let Pat::Ident(id) = &decl.name {
      if let Some(init) = &decl.init {
        if let Expr::Ident(init_id) = init.as_ref() {
          if init_id.sym.as_ref().eq("exports") {
            self.add_exports_alias(id.sym.as_ref());
          }
        } else if is_member(init, "module", "exports") {
          self.add_exports_alias(id.id.sym.as_ref());
        } else if let Expr::Assign(assign) = init.as_ref() {
          if let Some(member) = get_member_expr_from_assign_target(&assign.left) {
            if is_member(&Expr::Member(member.clone()), "module", "exports") {
              self.add_exports_alias(id.id.sym.as_ref());
            }
          }
        }
      }
    }
  }

  fn is_exports_ident(&self, id: &str) -> bool {
    id.eq("exports") || self.exports_alias.contains(id)
  }

  fn is_exports_expr(&self, expr: &Expr) -> bool {
    match expr {
      // (0, exports)
      Expr::Seq(SeqExpr { exprs, .. }) => exprs.last().map_or(false, |expr| self.is_exports_expr(expr)),
      Expr::Ident(id) => self.is_exports_ident(id.sym.as_ref()),
      Expr::Member(_) => is_member(expr, "module", "exports"),
      Expr::Paren(ParenExpr { expr, .. }) => self.is_exports_expr(expr),
      _ => false,
    }
  }

  /// get the property name of a member expression, resolving computed names
  /// like `exports[key]` where `key` is a known string.
  fn get_prop_name(&self, prop: &MemberProp) -> Option<String> {
    match prop {
      MemberProp::Ident(prop) => Some(prop.sym.as_ref().into()),
      MemberProp::Computed(ComputedPropName { expr, .. }) => self.as_str(expr),
      _ => None,
    }
  }

  fn get_exports_prop_name(&self, expr: &Expr) -> Option<String> {
    if let Expr::Member(MemberExpr { obj, prop, .. }) = expr {
      if let Expr::Ident(obj) = obj.as_ref() {
        if self.is_exports_ident(obj.sym.as_ref()) {
          return self.get_prop_name(prop);
        }
      }
    }
    None
  }

  // exports.foo || (exports.foo = {})
  // foo = exports.foo || (exports.foo = {})
  fn get_export_name_from_bin_expr(&self, expr: &Expr) -> Option<String> {
    if let Expr::Assign(assign) = expr {
      return self.get_export_name_from_bin_expr(&assign.right);
    }

    if let Expr::Bin(bin) = expr {
      if bin.op == BinaryOp::LogicalOr {
        if let Some(member_prop_name) = self.get_exports_prop_name(&bin.left) {
          if let Expr::Paren(ParenExpr { expr, .. }) = bin.right.as_ref() {
            if let Expr::Assign(assign) = expr.as_ref() {
              if let AssignTarget::Simple(SimpleAssignTarget::Member(MemberExpr { obj, prop, .. })) = &assign.left {
                if let Expr::Ident(obj) = obj.as_ref() {
                  if self.is_exports_ident(obj.sym.as_ref()) {
                    if let Some(prop_name) = self.get_prop_name(prop) {
                      if prop_name.eq(&member_prop_name) {
                        return Some(member_prop_name);
                      }
                    }
                  }
                }
              }
            }
          }
        }
      }
    }

    None
  }

  fn get_exports_from_assign(&mut self, assign: &'a AssignExpr) {
    if assign.op == AssignOp::Assign {
      if let AssignTarget::Simple(simple) = &assign.left {
        if let SimpleAssignTarget::Member(MemberExpr { obj, prop, .. }) = simple {
          if let Some(prop) = self.get_prop_name(prop) {
            match obj.as_ref() {
              Expr::Ident(obj) => {
                let obj_name = obj.sym.as_ref();
                if self.is_exports_ident(obj_name) {
                  // exports.foo = 'bar'
                  self.named_exports.insert(prop);
                  if let Expr::Assign(right_as_assign) = assign.right.as_ref() {
                    self.get_exports_from_assign(right_as_assign);
                  }
                  return;
                } else if obj_name.eq("module") && prop.eq("exports") {
                  // module.exports = ??
                  self.replace_exports_from_expr(&assign.right);
                  return;
                }
              }
              // module.exports.foo = 'bar'
              Expr::Member(_) => {
                if is_member(obj, "module", "exports") {
                  self.named_exports.insert(prop);
                  if let Expr::Assign(right_as_assign) = assign.right.as_ref() {
                    self.get_exports_from_assign(right_as_assign);
                  }
                  return;
                }
              }
              _ => {}
            }
          }
        } else if let SimpleAssignTarget::Ident(id) = simple {
          if self.is_exports_ident(id.sym.as_ref()) {
            // exports = ??
            self.replace_exports_from_expr(&assign.right);
            return;
          }
        }
      }
      if let Some(name) = self.get_export_name_from_bin_expr(&assign.right) {
        self.named_exports.insert(name);
      }
    }
  }

  fn get_webpack_exports(&mut self, stmts: &[Stmt], webpack_require_sym: &str, first_stmt_index: usize) {
    stmts.iter().skip(first_stmt_index).take(8).find(|stmt| match stmt {
      Stmt::Expr(ExprStmt { expr, .. }) => {
        if let Expr::Seq(SeqExpr { exprs, .. }) = expr.as_ref() {
          let mut found_webpack_require_exprs = false;
          for expr in exprs {
            if let Expr::Call(call) = expr.as_ref() {
              if let Some(Expr::Member(MemberExpr { obj, prop, .. })) = with_expr_callee(call) {
                if let (Expr::Ident(Ident { sym: obj_sym, .. }), MemberProp::Ident(IdentName { sym: prop_sym, .. })) =
                  (obj.as_ref(), prop)
                {
                  if obj_sym.as_ref() != webpack_require_sym {
                    return false;
                  }
                  let prop_sym_ref = prop_sym.as_ref();

                  if prop_sym_ref.eq("r") {
                    self.named_exports.insert("__esModule".to_owned());
                    found_webpack_require_exprs = true;
                  }
                  if prop_sym_ref.eq("d") {
                    if let Some(ExprOrSpread { expr, .. }) = call.args.get(1) {
                      if let Expr::Object(ObjectLit { props, .. }) = expr.as_ref() {
                        for prop in props {
                          if let PropOrSpread::Prop(prop) = prop {
                            if let Prop::KeyValue(KeyValueProp {
                              key: PropName::Ident(IdentName { sym, .. }),
                              ..
                            }) = prop.as_ref()
                            {
                              self.named_exports.insert(sym.as_ref().to_owned());
                              found_webpack_require_exprs = true;
                            }
                          }
                        }
                      }
                    }
                  }
                }
              }
            }
          }
          found_webpack_require_exprs
        } else {
          false
        }
      }
      _ => false,
    });
  }

  fn get_webpack_require_props_from_props(&self, props: &[PropOrSpread]) -> i32 {
    props
      .iter()
      .map(|prop| match prop {
        PropOrSpread::Prop(prop) => match prop.as_ref() {
          Prop::KeyValue(KeyValueProp {
            key: PropName::Ident(IdentName { sym, .. }),
            ..
          }) => {
            let sym_ref = sym.as_ref();
            if sym_ref.eq("r") || sym_ref.eq("d") {
              1
            } else {
              0
            }
          }
          _ => 0,
        },
        _ => 0,
      })
      .sum()
  }

  fn get_webpack_require_props_from_stmts(&self, stmts: &[Stmt], webpack_require_sym: &str) -> i32 {
    let count_assign = |left: &AssignTarget| -> i32 {
      if let AssignTarget::Simple(SimpleAssignTarget::Member(MemberExpr {
        obj,
        prop: MemberProp::Ident(IdentName { sym: prop_sym, .. }),
        ..
      })) = left
      {
        if let Expr::Ident(Ident { sym, .. }) = obj.as_ref() {
          if sym.as_ref() == webpack_require_sym {
            let prop_sym_ref = prop_sym.as_ref();
            if prop_sym_ref.eq("r") || prop_sym_ref.eq("d") {
              return 1;
            }
          }
        }
      }
      0
    };
    stmts
      .iter()
      .map(|stmt| {
        if let Stmt::Expr(ExprStmt { expr, .. }) = stmt {
          match expr.as_ref() {
            Expr::Seq(SeqExpr { exprs, .. }) => exprs
              .iter()
              .map(|expr| match expr.as_ref() {
                Expr::Assign(AssignExpr {
                  op: AssignOp::Assign,
                  left,
                  ..
                }) => count_assign(left),
                Expr::Call(call) => match get_iife_body(call) {
                  Some(FnBody::Block(body)) => self.get_webpack_require_props_from_stmts(body, webpack_require_sym),
                  _ => 0,
                },
                _ => 0,
              })
              .sum(),
            Expr::Assign(AssignExpr {
              op: AssignOp::Assign,
              left,
              ..
            }) => count_assign(left),
            _ => 0,
          }
        } else {
          0
        }
      })
      .sum()
  }

  // function (e, t, r) {
  //   "use strict";
  //   r.r(t), r.d(t, "named", (function () { return n }));
  //   var n = "named-export";
  //   t.default = "default-export";
  // }
  fn get_webpack4_exports(&mut self, expr: &Expr, webpack_exports_sym: &str, webpack_require_sym: Option<&str>) {
    match expr {
      Expr::Seq(SeqExpr { exprs, .. }) => {
        for expr in exprs {
          self.get_webpack4_exports(expr, webpack_exports_sym, webpack_require_sym)
        }
      }
      Expr::Call(call) => {
        if let Some(webpack_require_sym) = webpack_require_sym {
          if let Some(Expr::Member(MemberExpr { obj, prop, .. })) = with_expr_callee(call) {
            if let (Expr::Ident(Ident { sym: obj_sym, .. }), MemberProp::Ident(IdentName { sym: prop_sym, .. })) =
              (obj.as_ref(), prop)
            {
              if obj_sym.as_ref().eq(webpack_require_sym) && prop_sym.as_ref().eq("r") {
                self.named_exports.insert("__esModule".to_owned());
              }
              if obj_sym.as_ref().eq(webpack_require_sym) && prop_sym.as_ref().eq("d") {
                if let Some(ExprOrSpread { expr, .. }) = call.args.get(1) {
                  if let Expr::Lit(Lit::Str(Str { value, .. })) = expr.as_ref() {
                    self.named_exports.insert(value.to_string_lossy().into_owned());
                  }
                }
              }
            }
          }
        }
      }
      Expr::Assign(AssignExpr {
        left,
        op: AssignOp::Assign,
        ..
      }) => {
        // t.default = "default-export"
        if let AssignTarget::Simple(SimpleAssignTarget::Member(MemberExpr { obj, prop, .. })) = left {
          if let Expr::Ident(Ident { sym, .. }) = obj.as_ref() {
            if sym.as_ref().eq(webpack_exports_sym) {
              if let MemberProp::Ident(prop) = prop {
                if prop.sym.as_ref().eq("default") {
                  self.named_exports.insert("default".to_owned());
                }
              }
            }
          }
        }
      }
      _ => {}
    }
  }

  /// returns the factory body of an UMD wrapper call, with the factory param that receives the exports object.
  fn is_umd_iife_call(&self, call: &'a CallExpr) -> Option<(FnBody<'a>, Option<&'a str>)> {
    if call.args.len() != 2 {
      return None;
    }
    let mut factory = call.args[1].expr.as_ref();
    if let Expr::Paren(ParenExpr { expr, .. }) = factory {
      factory = expr;
    }
    let body = match factory {
      Expr::Fn(func) => func.function.body.as_ref().map(|body| FnBody::Block(&body.stmts)),
      Expr::Arrow(arrow) => Some(arrow_body(arrow)),
      _ => None,
    }?;
    let mut callee = with_expr_callee(call)?;
    if let Expr::Paren(ParenExpr { expr, .. }) = callee {
      callee = expr;
    }
    match callee {
      Expr::Fn(func) => {
        let params = &func.function.params;
        if params.len() == 2 && pat_is_ident(&params[0].pat, "global") && pat_is_ident(&params[1].pat, "factory") {
          return Some((body, None));
        }
        if let Some(FunctionBody { stmts, .. }) = &func.function.body {
          if is_umd_checks(stmts) {
            // the factory receives the exports object as its first argument when it is called
            // like `factory(exports)`, but not with `module.exports = factory(require('dep'))`
            let factory_name = params.get(1).and_then(|param| match &param.pat {
              Pat::Ident(BindingIdent { id, .. }) => Some(id.sym.as_ref()),
              _ => None,
            });
            let exports_param = match factory_name {
              Some(name) if is_umd_factory_called_with_exports(stmts, name) => first_param_ident(factory),
              _ => None,
            };
            return Some((body, exports_param));
          }
        }
      }
      Expr::Arrow(arrow) => {
        // TODO: detect for minified umd, haven't seen any in the wild using arrow fns yet though
        if arrow.params.len() == 2
          && pat_is_ident(&arrow.params[0], "global")
          && pat_is_ident(&arrow.params[1], "factory")
        {
          return Some((body, None));
        }
      }
      _ => {}
    }
    None
  }

  // walk and mark idents
  fn walk_stmts<I>(&mut self, stmts: I) -> bool
  where
    I: Iterator<Item = &'a Stmt>,
  {
    for stmt in stmts {
      match stmt {
        Stmt::Decl(decl) => match decl {
          Decl::Var(var) => {
            for decl in &var.decls {
              self.mark_exports_alias_from_var_decl(decl);
              match &decl.name {
                Pat::Ident(BindingIdent { id, .. }) => {
                  let id = id.sym.as_ref();
                  if let Some(init) = &decl.init {
                    self.mark_ident(id, init);
                  } else {
                    self.set_ident(id, IdentKind::Unknown);
                  }
                }
                Pat::Object(ObjectPat { props, .. }) => {
                  let process_env_init = match &decl.init {
                    Some(init) => is_member(init, "process", "env"),
                    None => false,
                  };
                  if process_env_init {
                    for prop in props {
                      match prop {
                        ObjectPatProp::Assign(AssignPatProp { key, .. }) => {
                          let key = key.sym.as_ref();
                          if key.eq("NODE_ENV") {
                            self.set_ident(key, IdentKind::Lit(Lit::Str(quote_str(self.node_env.as_str()))));
                          }
                        }
                        ObjectPatProp::KeyValue(KeyValuePatProp { key, value, .. }) => {
                          let key = stringify_prop_name(key);
                          if let (Some(key), Pat::Ident(rename)) = (key, value.as_ref()) {
                            if key.eq("NODE_ENV") {
                              self.set_ident(
                                rename.id.sym.as_ref(),
                                IdentKind::Lit(Lit::Str(quote_str(self.node_env.as_str()))),
                              );
                            }
                          }
                        }
                        _ => {}
                      }
                    }
                  }
                }
                _ => {}
              }
            }
          }
          Decl::Fn(FnDecl { ident, function, .. }) => {
            if let Some(body) = &function.body {
              self.set_ident(
                ident.sym.as_ref(),
                IdentKind::Fn(FnDesc {
                  body: FnBody::Block(&body.stmts),
                  extends: vec![],
                }),
              );
            }
          }
          Decl::Class(ClassDecl { ident, class, .. }) => {
            self.set_ident(ident.sym.as_ref(), IdentKind::Class(class));
          }
          _ => {}
        },
        Stmt::Expr(ExprStmt { expr, .. }) => self.mark_idents_from_expr(expr),
        Stmt::Block(BlockStmt { stmts, .. }) => {
          if self.walk_stmts(stmts.iter()) {
            return true;
          }
        }
        Stmt::If(IfStmt { test, cons, alt, .. }) => {
          let returned = if self.is_true(test) {
            self.walk_stmts(std::iter::once(cons.as_ref()))
          } else if let Some(alt) = alt {
            self.walk_stmts(std::iter::once(alt.as_ref()))
          } else {
            false
          };
          if returned {
            return true;
          }
        }
        Stmt::Return(_) => return true,
        _ => {}
      }
    }
    false
  }

  fn mark_idents_from_expr(&mut self, expr: &'a Expr) {
    if let Expr::Assign(assign) = expr {
      if assign.op == AssignOp::Assign {
        if let AssignTarget::Simple(simple) = &assign.left {
          match simple {
            // var foo = 'boo'
            // foo = 'bar'
            SimpleAssignTarget::Ident(BindingIdent { id, .. }) => {
              let id = id.sym.as_ref();
              if self.idents.contains_key(id) {
                self.mark_ident(id, &assign.right)
              }
            }
            // var foo = {}
            // foo.bar = 'bar'
            SimpleAssignTarget::Member(MemberExpr { obj, prop, .. }) => {
              if let Some(key) = self.get_prop_name(prop) {
                if let Expr::Ident(obj_id) = obj.as_ref() {
                  self.extend_ident(obj_id.sym.as_ref(), key);
                }
              }
            }
            _ => {}
          }
        }
      }
    }
  }

  fn parse_expr(&mut self, expr: &'a Expr) {
    match expr {
      Expr::Seq(SeqExpr { exprs, .. }) => {
        for expr in exprs {
          self.parse_expr(expr);
        }
      }
      // exports.foo = 'bar'
      // module.exports.foo = 'bar'
      // module.exports = { foo: 'bar' }
      // module.exports = { ...require('a'), ...require('b') }
      // module.exports = require('lib')
      // exports = module.exports = { foo: 'bar' }
      // foo = exports.foo || (exports.foo = {})
      Expr::Assign(assign) => {
        self.get_exports_from_assign(assign);
      }
      // Object.defineProperty(exports, 'foo', { value: 'bar' })
      // Object.defineProperty((0, exports), 'foo', { value: 'bar' })
      // Object.defineProperty(module.exports, 'foo', { value: 'bar' })
      // Object.defineProperty(module, 'exports', { value: { foo: 'bar' }})
      // Object.assign(exports, { foo: 'bar' })
      // Object.assign(module.exports, { foo: 'bar' }, { ...require('a') }, require('b'))
      // Object.assign(module, { exports: { foo: 'bar' } })
      // Object.assign(module, { exports: require('lib') })
      // (function() { ... })()
      // (function() { ... }).call(this)
      // require("tslib").__exportStar(..., exports)
      // tslib.__exportStar(..., exports)
      // __exportStar(..., exports)
      Expr::Call(call) => {
        if is_object_static_method_call(call, "defineProperty") && call.args.len() >= 3 {
          let arg0 = &call.args[0];
          let arg1 = &call.args[1];
          let arg2 = &call.args[2];
          let is_module = is_module_ident(&arg0.expr);
          let is_exports = self.is_exports_expr(&arg0.expr);

          let name = self.as_str(&arg1.expr);
          let mut with_value_or_getter = false;
          let mut with_value: Option<&'a Expr> = None;
          if let Some(obj) = self.as_obj(&arg2.expr) {
            for prop in obj.props {
              if let PropOrSpread::Prop(prop) = prop {
                let key = match prop.as_ref() {
                  Prop::KeyValue(KeyValueProp { key, value, .. }) => {
                    let key = stringify_prop_name(key);
                    if let Some(key) = &key {
                      if key.eq("value") {
                        with_value = Some(value);
                      }
                    }
                    key
                  }
                  Prop::Method(MethodProp { key, .. }) => stringify_prop_name(key),
                  _ => None,
                };
                if let Some(key) = key {
                  if key.eq("value") || key.eq("get") {
                    with_value_or_getter = true;
                    break;
                  }
                }
              }
            }
            if !with_value_or_getter && obj.extends.iter().any(|key| key.eq("value") || key.eq("get")) {
              with_value_or_getter = true;
            }
          }
          if is_exports && with_value_or_getter {
            if let Some(name) = name {
              self.named_exports.insert(name);
            }
          }
          if is_module {
            if let Some(expr) = with_value {
              self.replace_exports_from_expr(expr);
            }
          }
        } else if is_object_static_method_call(call, "assign") && call.args.len() >= 2 {
          let is_module = is_module_ident(&call.args[0].expr);
          let is_exports = self.is_exports_expr(&call.args[0].expr);
          for arg in &call.args[1..] {
            if let Some(obj) = self.as_obj(&arg.expr) {
              if is_module {
                let mut with_exports: Option<&'a Expr> = None;
                for prop in obj.props {
                  if let PropOrSpread::Prop(prop) = prop {
                    if let Prop::KeyValue(KeyValueProp { key, value, .. }) = prop.as_ref() {
                      if let Some(key) = stringify_prop_name(key) {
                        if key.eq("exports") {
                          with_exports = Some(value);
                          break;
                        }
                      }
                    }
                  }
                }
                if let Some(exports_expr) = with_exports {
                  self.replace_exports_from_expr(exports_expr);
                }
              } else if is_exports {
                self.update_exports_from_object(&obj);
              }
            } else if let Some(reexport) = self.as_reexport(&arg.expr) {
              if is_exports {
                self.reexports.insert(reexport.path);
              }
            }
          }
        } else if is_tslib_export_star_call(call) && call.args.len() >= 2 {
          if self.is_exports_expr(&call.args[1].expr) {
            if let Some(obj) = self.as_obj(&call.args[0].expr) {
              self.update_exports_from_object(&obj);
            } else if let Some(reexport) = self.as_reexport(&call.args[0].expr) {
              self.reexports.insert(reexport.path);
            }
          }
        } else if is_export_call(call) && !call.args.is_empty() {
          if let Some(obj) = self.as_obj(&call.args[0].expr) {
            self.update_exports_from_object(&obj);
          } else if let Some(reexport) = self.as_reexport(&call.args[0].expr) {
            self.reexports.insert(reexport.path);
          }
        } else if let Some((body, exports_param)) = self.is_umd_iife_call(call) {
          self.walk_scope(body, false, exports_param);
        } else if let Some(body) = get_iife_body(call) {
          for arg in &call.args {
            if arg.spread.is_none() {
              // (function() { ... })(exports.foo || (exports.foo = {}))
              if let Some(name) = self.get_export_name_from_bin_expr(&arg.expr) {
                self.named_exports.insert(name);
              }
            }
          }
          self.walk_body(body, false);
        }
      }
      // ~function(){ ... }()
      // !(function(e, t) { ... })(this, (function (e) { ... }));
      Expr::Unary(UnaryExpr { op, arg, .. }) => {
        if let UnaryOp::Minus | UnaryOp::Plus | UnaryOp::Bang | UnaryOp::Tilde | UnaryOp::Void = op {
          if let Expr::Call(call) = arg.as_ref() {
            if let Some((body, exports_param)) = self.is_umd_iife_call(call) {
              self.walk_scope(body, false, exports_param);
            } else if let Some(body) = get_iife_body(call) {
              // (function() { ... })(exports.foo || (exports.foo = {}))
              for arg in &call.args {
                if arg.spread.is_none() {
                  if let Some(name) = self.get_export_name_from_bin_expr(&arg.expr) {
                    self.named_exports.insert(name);
                  }
                }
              }
              self.walk_body(body, false);
            }
          }
        }
      }
      // (function(){ ... }())
      Expr::Paren(ParenExpr { expr, .. }) => {
        self.mark_idents_from_expr(expr);
        self.parse_expr(expr);
      }
      // 0 && (module.exports = { foo })
      // process.env.NODE_ENV === 'production' && (() => { module.exports = { foo } })()
      Expr::Bin(BinExpr { left, op, right, .. }) => {
        if matches!(op, BinaryOp::LogicalAnd) {
          if let Expr::Call(call) = right.as_ref() {
            if let Some(body) = get_iife_body(call) {
              if self.is_true(left) {
                for arg in &call.args {
                  if arg.spread.is_none() {
                    // (function() { ... })(exports.foo || (exports.foo = {}))
                    if let Some(name) = self.get_export_name_from_bin_expr(&arg.expr) {
                      self.named_exports.insert(name);
                    }
                  }
                }
                self.walk_body(body, false);
              }
            }
          } else if let Expr::Paren(paren) = right.as_ref() {
            if let Expr::Assign(assign) = paren.expr.as_ref() {
              if is_lit_number(left) || self.is_true(left) {
                self.get_exports_from_assign(assign);
              }
            }
          }
        }
      }
      _ => {}
    }
  }

  // return function(){ ... }([ ...webpack modules... ])   (webpack 4 minified umd)
  // return (() => { ... })()                              (webpack 5 minified umd)
  fn parse_return_stmt_arg(&mut self, arg: &'a Expr) {
    let Expr::Call(call) = arg else {
      return;
    };
    match with_expr_callee(call) {
      Some(Expr::Fn(FnExpr { function, .. })) => {
        if let Some(FunctionBody { stmts, .. }) = &function.body {
          let mut check_function = |function: &Function| {
            if let Some(FunctionBody { stmts, .. }) = &function.body {
              if let Some(Stmt::If(IfStmt { cons, .. })) = stmts.first() {
                if let Stmt::Return(ReturnStmt { arg: Some(arg), .. }) = cons.as_ref() {
                  if let Expr::Member(MemberExpr {
                    prop: MemberProp::Ident(prop),
                    ..
                  }) = arg.as_ref()
                  {
                    if prop.sym.as_ref().eq("exports") {
                      if call.args.len() != 1 {
                        return;
                      }
                      if let Some(ExprOrSpread { expr, .. }) = call.args.first() {
                        if let Expr::Array(ArrayLit { elems, .. }) = expr.as_ref() {
                          for elem in elems.iter().flatten() {
                            if let Expr::Fn(FnExpr { function, .. }) = elem.expr.as_ref() {
                              if let Some(FunctionBody { stmts, .. }) = &function.body {
                                let params = &function.params;
                                if let Some(Param {
                                  pat:
                                    Pat::Ident(BindingIdent {
                                      id:
                                        Ident {
                                          sym: webpack_exports_sym,
                                          ..
                                        },
                                      ..
                                    }),
                                  ..
                                }) = params.get(1)
                                {
                                  let webpack_require_sym = match params.get(2) {
                                    Some(Param {
                                      pat:
                                        Pat::Ident(BindingIdent {
                                          id: Ident { sym, .. }, ..
                                        }),
                                      ..
                                    }) => Some(sym.as_ref()),
                                    _ => None,
                                  };
                                  for stmt in stmts {
                                    if let Stmt::Expr(ExprStmt { expr, .. }) = stmt {
                                      self.get_webpack4_exports(expr, webpack_exports_sym.as_ref(), webpack_require_sym)
                                    }
                                  }
                                }
                              }
                            }
                          }
                        }
                      }
                    }
                  }
                }
              }
            }
          };
          if let Some(Stmt::Decl(Decl::Fn(FnDecl { function, .. }))) = stmts.first() {
            check_function(function);
          } else if let Some(Stmt::Decl(Decl::Fn(FnDecl { function, .. }))) = stmts.get(1) {
            check_function(function);
          }
        }
      }
      Some(Expr::Paren(ParenExpr { expr, .. })) => {
        if let Expr::Arrow(ArrowExpr { body, .. }) = expr.as_ref() {
          if let ArrowFunctionBody::FunctionBody(FunctionBody { stmts, .. }) = body.as_ref() {
            self.parse_webpack5_factory_stmts(stmts);
          }
        }
      }
      _ => {}
    }
  }

  fn parse_webpack5_factory_stmts(&mut self, stmts: &'a [Stmt]) {
    let first_stmt_index = match stmts.first() {
      Some(Stmt::Expr(ExprStmt { expr, .. })) => match expr.as_ref() {
        Expr::Lit(Lit::Str(Str { value, .. })) => {
          if value.to_string_lossy().eq("use strict") {
            1
          } else {
            0
          }
        }
        _ => 0,
      },
      _ => 0,
    };

    // var t = { d: ..., r: ... }
    if let Some(Stmt::Decl(Decl::Var(var_decl))) = stmts.get(first_stmt_index) {
      if let Some(VarDeclarator {
        name:
          Pat::Ident(BindingIdent {
            id: Ident {
              sym: webpack_require_sym,
              ..
            },
            ..
          }),
        init: Some(init),
        ..
      }) = var_decl.decls.first()
      {
        if let Expr::Object(ObjectLit { props, .. }) = init.as_ref() {
          if self.get_webpack_require_props_from_props(props) == 2 {
            self.get_webpack_exports(stmts, webpack_require_sym.as_ref(), first_stmt_index + 1);
          }
        }
      }
    }

    // function t(...) { ... } with `t.d = ...` / `t.r = ...`
    if let Some(Stmt::Decl(Decl::Fn(FnDecl {
      ident: Ident {
        sym: webpack_require_sym,
        ..
      },
      ..
    }))) = stmts.get(first_stmt_index + 1)
    {
      if self.get_webpack_require_props_from_stmts(stmts, webpack_require_sym.as_ref()) == 2 {
        if let Some(Stmt::Return(ReturnStmt { arg: Some(arg), .. })) = stmts.last() {
          if let Expr::Seq(SeqExpr { exprs, .. }) = arg.as_ref() {
            if let Some(Expr::Call(call)) = exprs.first().map(|expr| expr.as_ref()) {
              if let Some(FnBody::Block(module_stmts)) = get_iife_body(call) {
                self.get_webpack_exports(module_stmts, webpack_require_sym.as_ref(), 0);
              }
            }
          }
        }
      }
    }

    // return (function(){ var n = e; ... }(), e)
    if let Some(Stmt::Return(ReturnStmt { arg: Some(arg), .. })) = stmts.last() {
      if let Expr::Seq(SeqExpr { exprs, .. }) = arg.as_ref() {
        if let (Some(module_iife_expr), Some(module_exports_expr)) = (exprs.first(), exprs.last()) {
          if let (
            Expr::Call(module_iife_call),
            Expr::Ident(Ident {
              sym: module_exports_sym,
              ..
            }),
          ) = (module_iife_expr.as_ref(), module_exports_expr.as_ref())
          {
            if let Some(FnBody::Block(module_stmts)) = get_iife_body(module_iife_call) {
              for i in 0..2 {
                if let Some(Stmt::Decl(Decl::Var(var_decl))) = module_stmts.get(i) {
                  if let Some(VarDeclarator {
                    name:
                      Pat::Ident(BindingIdent {
                        id: Ident { sym, .. }, ..
                      }),
                    init: Some(init),
                    ..
                  }) = var_decl.decls.first()
                  {
                    if let Expr::Ident(Ident { sym: init_sym, .. }) = init.as_ref() {
                      if init_sym == module_exports_sym {
                        self.walk_scope(FnBody::Block(module_stmts), false, Some(sym.as_ref()));
                        return;
                      }
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
  }

  pub fn walk<I>(&mut self, stmts: I, as_fn: bool)
  where
    I: Iterator<Item = &'a Stmt> + Clone,
  {
    self.walk_stmts(stmts.clone());

    // check exports (as function)
    if as_fn {
      for stmt in stmts {
        if self.fn_returned {
          break;
        }
        match stmt {
          Stmt::Block(BlockStmt { stmts, .. }) => {
            self.walk_body(FnBody::Block(stmts), true);
          }
          Stmt::If(IfStmt { test, cons, alt, .. }) => {
            if self.is_true(test) {
              self.walk_body(FnBody::Block(std::slice::from_ref(cons.as_ref())), true);
            } else if let Some(alt) = alt {
              self.walk_body(FnBody::Block(std::slice::from_ref(alt.as_ref())), true);
            }
          }
          Stmt::Return(ReturnStmt { arg, .. }) => {
            self.fn_returned = true;
            if let Some(arg) = arg {
              self.replace_exports_from_expr(arg);
            }
          }
          _ => {}
        }
      }
      return;
    }

    // check exports
    for stmt in stmts {
      match stmt {
        // var foo = exports.foo || (exports.foo = {})
        // var foo = exports.foo = "bar"
        Stmt::Decl(Decl::Var(var)) => {
          for decl in var.decls.iter() {
            self.mark_exports_alias_from_var_decl(decl);
            if let Some(init_expr) = &decl.init {
              if let Some(name) = self.get_export_name_from_bin_expr(init_expr) {
                self.named_exports.insert(name);
              } else {
                self.parse_expr(init_expr);
              }
            }
          }
        }
        Stmt::Expr(ExprStmt { expr, .. }) => self.parse_expr(expr),
        Stmt::Block(BlockStmt { stmts, .. }) => {
          self.walk_body(FnBody::Block(stmts), false);
        }
        Stmt::If(IfStmt { test, cons, alt, .. }) => {
          if self.is_true(test) {
            self.walk_body(FnBody::Block(std::slice::from_ref(cons.as_ref())), false);
          } else if let Some(alt) = alt {
            self.walk_body(FnBody::Block(std::slice::from_ref(alt.as_ref())), false);
          }
        }
        Stmt::Return(ReturnStmt { arg: Some(arg), .. }) => self.parse_return_stmt_arg(arg),
        _ => {}
      }
    }
  }

  fn walk_body(&mut self, body: FnBody<'a>, as_fn: bool) {
    self.walk_scope(body, as_fn, None);
  }

  /// walk a body in a new scope, the idents and exports aliases declared in it are dropped afterwards.
  fn walk_scope(&mut self, body: FnBody<'a>, as_fn: bool, exports_alias: Option<&str>) {
    let id = match body {
      FnBody::Block(stmts) => (stmts.as_ptr() as usize, stmts.len()),
      FnBody::Expr(expr) => (expr as *const Expr as usize, usize::MAX),
    };
    // walking a body that is being walked means a recursive call, e.g. `function f() { return f() }`
    if self.walking.contains(&id) {
      return;
    }
    self.walking.push(id);
    let undo_len = self.undo.len();
    let call_mode = std::mem::replace(&mut self.call_mode, false);
    self.fn_returned = false;
    if let Some(name) = exports_alias {
      self.add_exports_alias(name);
    }
    match body {
      FnBody::Block(stmts) => self.walk(stmts.iter(), as_fn),
      // an expression-bodied arrow behaves like `{ return expr }`
      FnBody::Expr(expr) => {
        if as_fn {
          self.fn_returned = true;
          self.replace_exports_from_expr(expr);
        } else {
          self.parse_return_stmt_arg(expr);
        }
      }
    }
    self.call_mode = call_mode;
    self.rollback(undo_len);
    self.walking.pop();
  }
}

fn is_module_ident(expr: &Expr) -> bool {
  match expr {
    Expr::Ident(id) => id.sym.as_ref().eq("module"),
    _ => false,
  }
}

fn is_member(expr: &Expr, obj_name: &str, prop_name: &str) -> bool {
  if let Some(member_prop_name) = get_member_prop_name(expr, obj_name) {
    return member_prop_name.eq(prop_name);
  }
  false
}

fn is_member_member(expr: &Expr, obj_name: &str, middle_obj_name: &str, prop_name: &str) -> bool {
  if let Expr::Member(MemberExpr { obj, prop, .. }) = expr {
    if is_member(obj, obj_name, middle_obj_name) {
      if let Some(name) = get_static_prop_name(prop) {
        return name.eq(prop_name);
      }
    }
  }
  false
}

fn with_expr_callee(call: &CallExpr) -> Option<&Expr> {
  match &call.callee {
    Callee::Expr(callee) => Some(callee.as_ref()),
    _ => None,
  }
}

// require('lib')
fn with_require_call(call: &CallExpr) -> Option<String> {
  if let Some(Expr::Ident(id)) = with_expr_callee(call) {
    if id.sym.as_ref().eq("require") && !call.args.is_empty() {
      return match call.args[0].expr.as_ref() {
        Expr::Lit(Lit::Str(Str { value, .. })) => Some(value.to_string_lossy().into_owned()),
        _ => None,
      };
    }
  };
  None
}

// match:
// Object.defineProperty()
// Object.assign()
fn is_object_static_method_call(call: &CallExpr, method: &str) -> bool {
  if let Some(callee) = with_expr_callee(call) {
    return is_member(callee, "Object", method);
  }
  false
}

fn pat_is_ident(pat: &Pat, name: &str) -> bool {
  matches!(pat, Pat::Ident(bid) if bid.id.sym.as_ref() == name)
}

fn first_param_ident(func: &Expr) -> Option<&str> {
  match func {
    Expr::Fn(func) => func.function.params.first().and_then(|param| match &param.pat {
      Pat::Ident(BindingIdent { id, .. }) => Some(id.sym.as_ref()),
      _ => None,
    }),
    Expr::Arrow(arrow) => arrow.params.first().and_then(|pat| match pat {
      Pat::Ident(BindingIdent { id, .. }) => Some(id.sym.as_ref()),
      _ => None,
    }),
    _ => None,
  }
}

fn is_string_literal(expr: &Expr, value: &str) -> bool {
  match expr {
    Expr::Lit(Lit::Str(Str {
      value: literal_value, ..
    })) => literal_value.eq(value),
    _ => false,
  }
}

fn is_lit_number(expr: &Expr) -> bool {
  matches!(expr, Expr::Lit(Lit::Num(_)))
}

fn lit_is_true(lit: &Lit) -> bool {
  match lit {
    Lit::Bool(Bool { value, .. }) => *value,
    Lit::Str(Str { value, .. }) => !value.is_empty(),
    Lit::Null(_) => false,
    Lit::Num(Number { value, .. }) => *value != 0.0 && !value.is_nan(),
    Lit::BigInt(bigint) => bigint.value.bits() != 0,
    _ => true,
  }
}

fn is_typeof(expr: &Expr, identifier: &str) -> bool {
  match expr {
    Expr::Unary(UnaryExpr {
      arg,
      op: UnaryOp::TypeOf,
      ..
    }) => match arg.as_ref() {
      Expr::Ident(ident) => ident.sym.eq(identifier),
      _ => false,
    },
    _ => false,
  }
}

fn is_umd_exports_check(expr: &Expr) -> bool {
  match expr {
    Expr::Bin(BinExpr { left, right, op, .. }) => match op {
      BinaryOp::EqEq | BinaryOp::EqEqEq => {
        (is_typeof(left, "exports") && is_string_literal(right, "object"))
          || (is_typeof(right, "exports") && is_string_literal(left, "object"))
      }
      _ => false,
    },
    _ => false,
  }
}

fn is_umd_module_check(expr: &Expr) -> bool {
  match expr {
    Expr::Bin(BinExpr { left, right, op, .. }) => match op {
      BinaryOp::EqEq | BinaryOp::EqEqEq => {
        (is_typeof(left, "module") && is_string_literal(right, "object"))
          || (is_typeof(right, "module") && is_string_literal(left, "object"))
      }
      BinaryOp::NotEq | BinaryOp::NotEqEq => {
        (is_typeof(left, "module") && is_string_literal(right, "undefined"))
          || (is_typeof(right, "module") && is_string_literal(left, "undefined"))
      }
      _ => false,
    },
    _ => false,
  }
}

fn is_umd_define_check(expr: &Expr) -> bool {
  match expr {
    Expr::Bin(BinExpr { left, right, op, .. }) => match op {
      BinaryOp::EqEq | BinaryOp::EqEqEq => {
        (is_typeof(left, "define") && is_string_literal(right, "function"))
          || (is_typeof(right, "define") && is_string_literal(left, "function"))
      }
      _ => false,
    },
    _ => false,
  }
}

// if ('object' == typeof exports && 'object' == typeof module)
// module.exports = t(require('react'));
// else if ('function' == typeof define && define.amd) define(['react'], t);
// else {
// var r = 'object' == typeof exports ? t(require('react')) : t(e.react);
// for (var n in r) ('object' == typeof exports ? exports : e)[n] = r[n];
// }
fn is_umd_checks(stmts: &[Stmt]) -> bool {
  let (test, alt_test) = match stmts.first() {
    // !(function (e, t) {
    //   "object" == typeof exports && "undefined" != typeof module
    //     ? t(exports)
    //     : "function" == typeof define && define.amd
    //     ? define(["exports"], t)
    //     : t(((e = ...).rudderanalytics = {}));
    // })(this, function (e) {
    Some(Stmt::Expr(ExprStmt { expr, .. })) => match expr.as_ref() {
      Expr::Cond(CondExpr { test, alt, .. }) => {
        let alt_test = match alt.as_ref() {
          Expr::Cond(CondExpr { test, .. }) => Some(test.as_ref()),
          _ => None,
        };
        (Some(test.as_ref()), alt_test)
      }
      _ => (None, None),
    },
    Some(Stmt::If(IfStmt { test, alt, .. })) => {
      let alt_test = match alt {
        Some(alt_stmt) => match alt_stmt.as_ref() {
          Stmt::If(IfStmt { test, .. }) => Some(test.as_ref()),
          _ => None,
        },
        _ => None,
      };
      (Some(test.as_ref()), alt_test)
    }
    _ => (None, None),
  };
  let (Some(test), Some(alt_test)) = (test, alt_test) else {
    return false;
  };
  if let Expr::Bin(BinExpr { left, right, op, .. }) = test {
    if matches!(op, BinaryOp::LogicalAnd)
      && ((is_umd_exports_check(left) && is_umd_module_check(right))
        || (is_umd_exports_check(right) && is_umd_module_check(left)))
    {
      if let Expr::Bin(BinExpr { left, op, .. }) = alt_test {
        return matches!(op, BinaryOp::LogicalAnd) && is_umd_define_check(left);
      }
    }
  }
  false
}

// match the commonjs branch of the umd checks calling `factory(exports, ...)`:
// "object" == typeof exports && "undefined" != typeof module ? t(exports) : ...
// if ("object" == typeof exports && "object" == typeof module) { t(exports) } else ...
fn is_umd_factory_called_with_exports(stmts: &[Stmt], factory_name: &str) -> bool {
  fn is_call_with_exports(expr: &Expr, factory_name: &str) -> bool {
    match expr {
      Expr::Paren(ParenExpr { expr, .. }) => is_call_with_exports(expr, factory_name),
      Expr::Seq(SeqExpr { exprs, .. }) => exprs.iter().any(|expr| is_call_with_exports(expr, factory_name)),
      Expr::Call(call) => {
        matches!(with_expr_callee(call), Some(Expr::Ident(id)) if id.sym.as_ref() == factory_name)
          && matches!(call.args.first(), Some(ExprOrSpread { spread: None, expr }) if matches!(expr.as_ref(), Expr::Ident(id) if id.sym.as_ref() == "exports"))
      }
      _ => false,
    }
  }
  fn is_stmt_call_with_exports(stmt: &Stmt, factory_name: &str) -> bool {
    match stmt {
      Stmt::Expr(ExprStmt { expr, .. }) => is_call_with_exports(expr, factory_name),
      Stmt::Block(BlockStmt { stmts, .. }) => stmts.iter().any(|stmt| is_stmt_call_with_exports(stmt, factory_name)),
      _ => false,
    }
  }
  match stmts.first() {
    Some(Stmt::Expr(ExprStmt { expr, .. })) => match expr.as_ref() {
      Expr::Cond(CondExpr { cons, .. }) => is_call_with_exports(cons, factory_name),
      _ => false,
    },
    Some(Stmt::If(IfStmt { cons, .. })) => is_stmt_call_with_exports(cons, factory_name),
    _ => false,
  }
}

fn get_iife_body<'a>(call: &'a CallExpr) -> Option<FnBody<'a>> {
  let mut callee = with_expr_callee(call)?;
  callee = match callee {
    Expr::Paren(ParenExpr { expr, .. }) => expr,
    // (function() { ... }).call(this)
    Expr::Member(MemberExpr { obj, prop, .. }) => {
      match get_static_prop_name(prop) {
        Some(prop_name) if prop_name.eq("call") => {}
        _ => return None,
      }
      match obj.as_ref() {
        Expr::Paren(ParenExpr { expr, .. }) => expr,
        _ => return None,
      }
    }
    _ => callee,
  };
  match callee {
    Expr::Fn(func) => func.function.body.as_ref().map(|body| FnBody::Block(&body.stmts)),
    Expr::Arrow(arrow) => Some(arrow_body(arrow)),
    _ => None,
  }
}

fn is_export_call(call: &CallExpr) -> bool {
  match with_expr_callee(call) {
    Some(Expr::Ident(id)) => id.sym.as_ref().eq("__export"),
    _ => false,
  }
}

// match:
// require("tslib").__exportStar(..., exports)
// (0, require("tslib").__exportStar)(..., exports)
// const tslib = require("tslib"); (0, tslib.__exportStar)(..., exports)
// const {__exportStar} = require("tslib"); (0, __exportStar)(..., exports)
// const __exportStar = () => {}; __exportStar(..., exports)
fn is_tslib_export_star_call(call: &CallExpr) -> bool {
  fn is_export_star_callee(expr: &Expr) -> bool {
    match expr {
      Expr::Member(MemberExpr { prop, .. }) => match prop {
        MemberProp::Ident(prop) => prop.sym.as_ref().eq("__exportStar"),
        _ => false,
      },
      Expr::Ident(id) => id.sym.as_ref().eq("__exportStar"),
      _ => false,
    }
  }
  if let Some(callee) = with_expr_callee(call) {
    return match callee {
      Expr::Paren(ParenExpr { expr, .. }) => match expr.as_ref() {
        Expr::Seq(SeqExpr { exprs, .. }) => exprs.last().map_or(false, |last| is_export_star_callee(last)),
        expr => is_export_star_callee(expr),
      },
      callee => is_export_star_callee(callee),
    };
  }
  false
}

fn get_member_expr_from_assign_target(target: &AssignTarget) -> Option<&MemberExpr> {
  match target {
    AssignTarget::Simple(SimpleAssignTarget::Member(member)) => Some(member),
    _ => None,
  }
}

fn arrow_body<'a>(arrow: &'a ArrowExpr) -> FnBody<'a> {
  match arrow.body.as_ref() {
    ArrowFunctionBody::FunctionBody(FunctionBody { stmts, .. }) => FnBody::Block(stmts),
    ArrowFunctionBody::Expr(expr) => FnBody::Expr(expr),
  }
}

fn get_member_prop_name(expr: &Expr, obj_name: &str) -> Option<String> {
  if let Expr::Member(MemberExpr { obj, prop, .. }) = expr {
    if let Expr::Ident(obj) = obj.as_ref() {
      if obj.sym.as_ref().eq(obj_name) {
        return get_static_prop_name(prop);
      }
    }
  }
  None
}

fn get_class_static_names(class: &Class) -> Vec<String> {
  class
    .body
    .iter()
    .filter_map(|member| match member {
      ClassMember::ClassProp(prop) if prop.is_static => stringify_prop_name(&prop.key),
      ClassMember::Method(method) if method.is_static => stringify_prop_name(&method.key),
      _ => None,
    })
    .collect()
}

/// get the property name of a member expression, only for static names like
/// `foo.bar` and `foo["bar"]`.
fn get_static_prop_name(prop: &MemberProp) -> Option<String> {
  match prop {
    MemberProp::Ident(prop) => Some(prop.sym.as_ref().into()),
    MemberProp::Computed(ComputedPropName { expr, .. }) => match expr.as_ref() {
      Expr::Lit(Lit::Str(Str { value, .. })) => Some(value.to_string_lossy().into_owned()),
      _ => None,
    },
    _ => None,
  }
}

fn stringify_prop_name(name: &PropName) -> Option<String> {
  match name {
    PropName::Ident(id) => Some(id.sym.as_ref().into()),
    PropName::Str(Str { value, .. }) => Some(value.to_string_lossy().into_owned()),
    _ => None,
  }
}

fn quote_str(value: &str) -> Str {
  Str {
    span: DUMMY_SP,
    value: value.into(),
    raw: None,
  }
}
