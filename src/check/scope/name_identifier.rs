use parse::ScopedId;
use parse::ast::*;

use check::{ASTVisitor, ErrorCollector, CheckerError};
use check::scope::scope_builder::ScopeBuilder;

/// Establishes `ScopeId`s for identifiers of variables in expressions.
///
/// The `NameIdentifier` sets unique `ScopedId`s to `Identifier`s
/// throughout the AST. The first pass sets up items which can be used in
/// expressions, such as the names of functions or types. The second pass
/// sets up `Identifier`s within function bodies to both local variables and
/// globally-declard items.
#[derive(Debug, PartialEq, Clone)]
pub struct NameIdentifier<'err, 'builder> {
    /// Build up the map of all name declarations.
    builder: &'builder mut ScopeBuilder,
    /// Mutably borrow an ErrorCollector to push to while we're running.
    errors: &'err mut ErrorCollector,
    /// ID of the function or top-level scope we're in.
    current_id: ScopedId
}

impl<'err, 'builder> NameIdentifier<'err, 'builder> {
    pub fn new(errors: &'err mut ErrorCollector,
               builder: &'builder mut ScopeBuilder) -> NameIdentifier<'err, 'builder> {
        NameIdentifier {
            // The current ID can be left as default because it gets overridden
            // as soon as we visit an `Item`.
            builder, errors,
            current_id: ScopedId::default(),
        }
    }

    pub fn first_check_item(&self, item: &Item) {
        match item {

        }
    }
}

impl<'err, 'builder> ASTVisitor for NameIdentifier<'err, 'builder> {
    fn check_unit(&mut self, unit: &Unit) {
        // We increment the id first in `Unit` so that the first index of
        // an ident's ID is never 0.
        // This also means we're safe to check multiple units with one checker.
        self.current_id.increment();
        self.current_id.push();
        for item in unit.get_items() {
            self.check_item(item);
        }
        self.current_id.pop();
    }

    fn check_fn_declaration(&mut self, fn_decl: &FnDeclaration) {
        trace!("Checking fn {}", fn_decl.get_name());

        // If there's no ID on the function it's a dupe. Ignore for now.
        // In the future, adding more information to the ErrorCollector would
        // allow us to check variables inside the function, and report the error
        // futher out. If we did check this function def, at best we'd have
        // a bunch of GCC-style duplicated errors, at worst we could break
        // `ScopedId` invariants in the AST (i.e. IDs being unique). We take the
        // safe route for errors, i.e. not having them :( for now.
        // Duplicate function error is already handled in the ItemChecker.
        if fn_decl.get_ident().get_id() == ScopedId::default() {
            trace!("fn {} has no ScopedId, skipping!", fn_decl.get_name());
            return
        }

        // Save the top level ID when checking a new fn declaration.
        self.current_id = fn_decl.get_ident().get_id().clone();

        // Check in the functions params (not in the global scope)
        self.current_id.push();
        self.builder.new_scope();

        for param in fn_decl.get_args() {
            trace!("Checking {}'s param {}", fn_decl.get_name(), param.get_name());

            // Check for duplicate param names
            if let Some(declared_index) = self.builder.get(param.get_name()).cloned() {
                trace!("Encountered duplicate param {}", param.get_name());
                // TODO get previous declaration index
                // We can maintain this information in the builder and possibly pass it
                // onto the symbol checker to reduce the amount of repetition there.
                let err_text = format!("Argument {} is already declared", param.get_name());
                self.errors.add_error(CheckerError::new(
                    param.get_token().clone(), vec![], err_text
                ));
                // Skip adding a ScopedId to this param!
                continue
            }
            // Set the ScopedId of the param.
            let param_id = self.current_id.clone();
            self.current_id.increment();
            trace!("Created ID {:?} for fn {} arg {}",
                param_id, fn_decl.get_name(), param.get_name());
            self.builder.define_local(param.get_name().to_string(), param_id.clone());
            param.set_id(param_id);
        }

        // Immediately start checking function statements, instead of having the args
        // be in a separate scope. This isn't really needed, but having fewer scopes
        // could improve performance of smallvec. If we ever _use_ the scopes in
        // `ScopedId` for anything, it might be nicer to have them separate.
        for stmt in fn_decl.get_block().get_stmts() {
            self.check_stmt(&stmt);
        }
        // Don't need to clean up self.current_id in this checker.
    }

    fn check_block(&mut self, block: &Block) {
        trace!("Checking a block");
        self.current_id.push();
        self.builder.new_scope();
        for stmt in block.get_stmts() {
            self.check_statement(stmt);
        }
        self.builder.pop();
        self.current_id.pop();
        self.current_id.increment();
    }

    fn check_declaration(&mut self, decl: &Declaration) {
        // Check rvalue first
        self.check_expression(decl.get_value());
        trace!("Checking declaration of {}", decl.get_name());

        if let Some(declared_id) = self.builder.get(decl.get_name()) {
            // TODO reference the previous declaration
            let err_text = format!("Variable {} is already declared", decl.get_name());
            self.errors.add_error(CheckerError::new(
                decl.get_ident().get_token().clone(), vec![], err_text
            ));
            return
        }

        let decl_id = self.current_id.clone();
        self.current_id.increment();
        trace!("Created ID {:?} for variable {}", decl_id, decl.get_name());
    }

    fn check_fn_call(&mut self, call: &FnCall) {
        // Only check that the fn name exists, and recurse into the args.
        if let Some(fn_id) = self.table_builder.get(call.get_text()).cloned() {
            call.get_ident().set_id(fn_id);
            // TODO Args are not checked if fn name is not known!
            match *call.get_args() {
                FnCallArgs::SingleExpr(ref expr) => {
                    self.check_expression(expr);
                },
                FnCallArgs::Arguments(ref args) => {
                    for call_arg in args {
                        // Check expresions in args
                        if let Some(expr) = call_arg.get_expr() {
                            self.check_expression(expr);
                        }
                        // If using implicit names, need to check the
                        // name exists in _our_ scope first.
                        else {
                            self.check_var_ref(call_arg.get_name());
                        }
                    }
                }
            }
        }
        else {
            let err_text = format!("Unknown function {}", call.get_name());
            let err = CheckerError::new(call.get_token().clone(), vec![], err_text);
            self.errors.add_error(err);
        }
    }

    fn check_var_ref(&mut self, var_ref: &Identifier) {
        trace!("Checking reference to {}", var_ref.get_name());
        if let Some(index) = self.table_builder.get(var_ref.get_name()) {
            trace!("Found reference to {} with ID {}", var_ref.get_name(), index);
            var_ref.set_index(index.clone());
        }
    }
}