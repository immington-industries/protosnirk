use std::collections::HashMap;

use parse::{ASTVisitor, ScopeIndex, SymbolTable};
use parse::ast::*;
use compile::{LLVMContext, ModuleProvider};

use llvm_sys::{self, LLVMOpcode, LLVMRealPredicate};
use llvm_sys::prelude::*;
use llvm_sys::analysis::LLVMVerifierFailureAction;
use llvm_sys::core::LLVMFloatType;
use iron_llvm::{LLVMRef, LLVMRefCtor};
use iron_llvm::core::{Function, Builder};
use iron_llvm::core::basic_block::BasicBlock;
use iron_llvm::core::instruction::{PHINode, PHINodeRef};
use iron_llvm::core::value::{RealConstRef, FunctionRef, Value};
use iron_llvm::core::types::{RealTypeRef, FunctionTypeRef, FunctionTypeCtor, RealTypeCtor};
use iron_llvm::core::value::{RealConstCtor, ConstCtor, FunctionCtor};

pub struct ModuleCompiler<M: ModuleProvider> {
    module_provider: M,
    optimizations: bool,
    context: LLVMContext,
    ir_code: Vec<LLVMValueRef>,
    symbols: SymbolTable,
    scope_manager: HashMap<ScopeIndex, LLVMValueRef>
}
impl<M: ModuleProvider> ModuleCompiler<M> {
    pub fn new(symbols: SymbolTable, provider: M, optimizations: bool) -> ModuleCompiler<M> {
        ModuleCompiler {
            module_provider: provider,
            context: LLVMContext::new(),
            symbols: symbols,
            ir_code: Vec::with_capacity(1),
            scope_manager: HashMap::new(),
            optimizations: optimizations
        }
    }
    pub fn decompose(self) -> (M, LLVMContext, SymbolTable) {
        (self.module_provider, self.context, self.symbols)
    }

    // Extensions to the ASTVisitor

    fn check_if_block_valued(&mut self, if_block: &IfBlock) {
        debug_assert!(if_block.has_value(),
            "Valueless if block passed to `check_if_block_valued`: {:?}", if_block);
        let conditions_count = if_block.get_conditionals().len();
        let incoming_values = Vec::with_capacity(conditions_count);
        let incoming_edges = Vec::with_capacity(conditions_count);
        let mut else_if_blocks = Vec::with_capacity(conditions_count);

        // Create basic blocks to branch to in the if
        let mut function = self.context.builder().get_insert_block().get_parent();
        let mut end_block =
            function.append_basic_block_in_context(self.context.get_global_context_mut(), "ifv_end");
        // Optionally create else block
        let else_block = if if_block.has_else() {
            Some(function.append_basic_block_in_context(self.context.get_global_context_mut(), "ifv_else"))
        }
        else { None };

        let const_zero = RealConstRef::get(&unsafe { RealTypeRef::from_ref(LLVMFloatType())}, 0.0);

        for (ix, conditional) in if_block.get_conditionals().iter().enumerate() {
            debug_assert!(conditional.has_value(),
                "Valueless conditional passed to `check_if_block_valued`: {:?}", conditional);
            self.check_expression(conditional.get_condition());
            let then_block_name = format!("iv_then{}", ix);
            let mut then_block =
                function.append_basic_block_in_context(self.context.get_global_context_mut(), &then_block_name);
            let cond_expr = self.ir_code.pop()
                .expect("Did not get value from conditional");
            let cond_name = format!("iv_cond{}", ix);
            let cond_value = self.context.builder_mut()
                .build_fcmp(LLVMRealPredicate::LLVMRealOEQ, cond_expr, const_zero.to_ref(), &cond_name);

            // Last `else if` branches to else, if present
            if conditions_count > ix + 1 {

            }
            // There are more else ifs
            else {

            }
        }
    }

    fn check_if_block_unvalued(&mut self, if_block: &IfBlock) {

    }
}
impl<M:ModuleProvider> ASTVisitor for ModuleCompiler<M> {
    fn check_literal(&mut self, literal: &Literal) {
        trace!("Checking literal {}", literal.token);
        let float_type = RealTypeRef::get_float();
        debug_assert!(!float_type.to_ref().is_null());
        let float_value = literal.get_value();
        let literal_value = RealConstRef::get(&float_type, float_value);
        debug_assert!(!literal_value.to_ref().is_null());
        self.ir_code.push(literal_value.to_ref());
    }

    fn check_var_ref(&mut self, ident_ref: &Identifier) {
        trace!("Checking variable ref {}", ident_ref.get_name());
        let var_alloca = self.scope_manager.get(&ident_ref.get_index())
            .expect("Attempted to check var ref but had no alloca");
        let load_name = format!("load_{}", ident_ref.get_name());
        let mut builder = self.context.builder_mut();
        let var_load = builder.build_load(*var_alloca, &load_name);
        self.ir_code.push(var_load);
    }

    fn check_declaration(&mut self, decl: &Declaration) {
        trace!("Checking declaration for {}", decl.get_name());
        self.check_expression(decl.get_value());
        let decl_value = self.ir_code.pop()
            .expect("Did not have rvalue of declaration");
        let mut builder = self.context.builder_mut();
        let float_type = RealTypeRef::get_float();
        let alloca = builder.build_alloca(float_type.to_ref(), decl.get_name());
        self.scope_manager.insert(decl.ident.get_index(), alloca.to_ref());
        builder.build_store(decl_value, alloca);
    }

    fn check_assignment(&mut self, assign: &Assignment) {
        trace!("Checking assignment of {}", assign.lvalue.get_name());
        self.check_expression(&*assign.rvalue);
        let rvalue = self.ir_code.pop()
            .expect("Could not generate rvalue of assignment");
        let var_alloca = self.scope_manager.get(&assign.lvalue.get_index())
            .expect("Could not find existing var for assignment!");
        let mut builder = self.context.builder_mut();
        builder.build_store(rvalue, *var_alloca);
    }

    fn check_unary_op(&mut self, unary_op: &UnaryOperation) {
        debug_assert!(unary_op.operator == Operator::Subtraction,
            "Invalid unary operator {:?}", unary_op.operator);
        self.check_expression(&*unary_op.expression);
        let inner_value = self.ir_code.pop()
            .expect("Did not generate value inside unary op");
        let mut builder = self.context.builder_mut();
        let value = match unary_op.operator {
            Operator::Subtraction =>
                builder.build_neg(inner_value, "negate"),
            other => panic!("Invalid unary operator {:?}", other)
        };
        self.ir_code.push(value);
    }

    fn check_binary_op(&mut self, binary_op: &BinaryOperation) {
        trace!("Checking binary operation {:?}", binary_op.get_operator());
        trace!("Checking {:?} lvalue", binary_op.get_operator());
        self.check_expression(&*binary_op.left);
        let left_register = self.ir_code.pop()
            .expect("Could not generate lvalue of binary op");
        trace!("Checking {:?} rvalue", binary_op.get_operator());
        self.check_expression(&*binary_op.right);
        let right_register = self.ir_code.pop()
            .expect("Could not generate rvalue of binary op");
        let mut builder = self.context.builder_mut();
        trace!("Appending binary operation");
        use llvm_sys::LLVMRealPredicate::*;
        let bin_op_value = match binary_op.get_operator() {
            Operator::Addition =>
                builder.build_fadd(left_register, right_register, "add"),
            Operator::Subtraction =>
                builder.build_fsub(left_register, right_register, "sub"),
            Operator::Multiplication =>
                builder.build_fmul(left_register, right_register, "mul"),
            Operator::Division =>
                builder.build_binop(LLVMOpcode::LLVMFDiv, left_register, right_register, "div"),
            Operator::Modulus =>
                builder.build_frem(left_register, right_register, "rem"),
            // TODO binary operations should be handled seperately
            // when types are added
            Operator::Equality => {
                let eq = builder.build_fcmp(LLVMRealOEQ, left_register, right_register, "eqtmp");
                builder.build_ui_to_fp(eq, unsafe { LLVMFloatType() }, "eqcast")
            },
            Operator::NonEquality => {
                let neq = builder.build_fcmp(LLVMRealONE, left_register, right_register, "neqtmp");
                builder.build_ui_to_fp(neq, unsafe { LLVMFloatType() }, "neqcast")
            },
            Operator::LessThan => {
                let lt = builder.build_fcmp(LLVMRealOLT, left_register, right_register, "lttmp");
                builder.build_ui_to_fp(lt, unsafe { LLVMFloatType() }, "ltcast")
            },
            Operator::LessThanEquals => {
                let le = builder.build_fcmp(LLVMRealOLE, left_register, right_register, "letmp");
                builder.build_ui_to_fp(le, unsafe { LLVMFloatType() }, "lecast")
            },
            Operator::GreaterThan => {
                let gt = builder.build_fcmp(LLVMRealOGT, left_register, right_register, "gttmp");
                builder.build_ui_to_fp(gt, unsafe { LLVMFloatType() }, "gtcast")
            },
            Operator::GreaterThanEquals => {
                let ge = builder.build_fcmp(LLVMRealOGE, left_register, right_register, "getmp");
                builder.build_ui_to_fp(ge, unsafe { LLVMFloatType() }, "gecast")
            }
            Operator::Custom => panic!("Cannot handle custom operator")
        };
        self.ir_code.push(bin_op_value);
    }

    fn check_return(&mut self, return_: &Return) {
        trace!("Checking return statement");
        if let Some(ref return_expr) = return_.value {
            self.check_expression(&*return_expr);
            let return_val = self.ir_code.pop()
                .expect("Could not generate value of return");
            let mut builder = self.context.builder_mut();
            builder.build_ret(&return_val);
        }
        else {
            warn!("Empty return statement, appending ret void");
            let mut builder = self.context.builder_mut();
            // Hopefully doesn't happen, protosnirk doesn't support void types
            builder.build_ret_void();
        }
    }

    fn check_unit(&mut self, unit: &Unit) {
        trace!("Checking unit");
        let fn_ret_double = RealTypeRef::get_float().to_ref();
        let block_fn_type = FunctionTypeRef::get(&fn_ret_double, &mut [], false);
        trace!("Creating `fn main` definition");
        let mut fn_ref = FunctionRef::new(&mut self.module_provider.get_module_mut(),
            "main", &block_fn_type);
        let mut basic_block = fn_ref.append_basic_block_in_context(
            self.context.get_global_context_mut(), "entry");
        self.context.builder_mut().position_at_end(&mut basic_block);
        trace!("Positioned IR builder at the end of entry block, checking unit block");
        self.check_block(&unit.block);

        let mut builder = self.context.builder_mut();
        // We can auto-issue a return stmt if the ir_code hasn't been
        // consumed. Otherwise, we return 0f64.
        // If the last statement _was_ a return, it's just a redundant
        // return that llvm should optimize out.
        // This will also need to be fixed when allowing nested blocks.
        // This will be redone in the parsing stages.
        if let Some(remaining_expr) = self.ir_code.pop() {
            trace!("Found final expression, appending a return");
            builder.build_ret(&remaining_expr);
        }

        // Returns true if verification failed
        assert!(!fn_ref.verify(LLVMVerifierFailureAction::LLVMPrintMessageAction));
        if self.optimizations {
            trace!("Running optimizations");
            self.module_provider.get_pass_manager().run(&mut fn_ref);
        }
        // The final ir_code value should be a reference to the function
        self.module_provider.get_module()
            .verify(LLVMVerifierFailureAction::LLVMPrintMessageAction)
            .unwrap();
    }

    fn check_if_expr(&mut self, if_expr: &IfExpression) {
        // Build conditional expr
        self.check_expression(if_expr.get_condition());
        let condition_expr = self.ir_code.pop()
            .expect("Did not get value from if conditional");
        let const_zero = RealConstRef::get(&unsafe { RealTypeRef::from_ref(LLVMFloatType()) }, 0.0);
        // hack: compare it to 0, due to lack of booleans right now
        let condition = self.context.builder_mut()
            .build_fcmp(LLVMRealPredicate::LLVMRealOEQ, condition_expr, const_zero.to_ref(), "ife_cond");
        // Create basic blocks in the function
        let mut function = self.context.builder().get_insert_block().get_parent();
        let mut then_block =
            function.append_basic_block_in_context(self.context.get_global_context_mut(), "ife_then");
        let mut else_block =
            function.append_basic_block_in_context(self.context.get_global_context_mut(), "ife_else");
        let mut end_block =
            function.append_basic_block_in_context(self.context.get_global_context_mut(), "ife_end");
        // Branch off of the `== 0` comparison
        self.context.builder_mut().build_cond_br(condition, &then_block, &else_block);

        // Emit the then code
        self.context.builder_mut().position_at_end(&mut then_block);
        self.check_expression(if_expr.get_true_expr());
        let then_value = self.ir_code.pop()
            .expect("Did not get IR value from visiting `then` clause of if expression");
        self.context.builder_mut().build_br(&end_block);
        let then_end_block = self.context.builder_mut().get_insert_block();

        // Emit the else code
        self.context.builder_mut().position_at_end(&mut else_block);
        self.check_expression(if_expr.get_else());
        let else_value = self.ir_code.pop()
            .expect("Did not get IR value from visiting `else` clause of if expression");
        self.context.builder_mut().build_br(&end_block);
        let else_end_block = self.context.builder_mut().get_insert_block();

        self.context.builder_mut().position_at_end(&mut end_block);
        let mut phi = unsafe {
            PHINodeRef::from_ref(self.context.builder_mut().build_phi(LLVMFloatType(), "ifephi"))
        };

        phi.add_incoming(vec![then_value].as_mut_slice(), vec![then_end_block].as_mut_slice());
        phi.add_incoming(vec![else_value].as_mut_slice(), vec![else_end_block].as_mut_slice());
        self.ir_code.push(phi.to_ref());
    }

    fn check_if_block(&mut self, if_block: &IfBlock) {
        // if cond
        //     block
        // else
        //     block
        // if cond
        //     block
        // else if otherCond
        //     otherBlock
        // else
        //     otherBlock
        // - `else if`s are stored in a list instead of a tree
        // - `if` may or may not have value
        // - `else` may or may not be present
        // - the else if conditionals all have outgoing edges to either the last else or end if.

        // It's important to differentiate between valued and unvalued conditionals
        // because valued if needs to use SSA and phi but unvalued should just be a jump.
        // There will need to be a higher level pass for determining return types. That pass will
        // determine if the last value in a function is the return value, given the function's
        // prototype. It will be determining that all code paths produce a value. It's unlikely
        // that we'll need to differentiate if blocks with value VS if blocks without value
        // at the AST level, it'll probably be encoded during that pass in the symbol table/with
        // type info in typecheck.

        if if_block.has_value() {
            self.check_if_block_valued(if_block)
        }
        else {
            self.check_if_block_unvalued(if_block)
        }
    }


    fn check_block(&mut self, block: &Block) {
        trace!("Checking block");
        for stmt in &block.statements {
            self.check_statement(stmt)
        }
    }
}
