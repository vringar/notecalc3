use std::ops::Not;
use std::ops::{BitXor, Shl};
use std::ops::{Neg, Shr};

use crate::matrix::MatrixData;
use crate::token_parser::{debug_print, OperatorTokenType, Token, TokenType, UnitTokenType};
use crate::units::consts::EMPTY_UNIT_DIMENSIONS;
use crate::units::units::{UnitOutput, Units, MAX_UNIT_COUNT};
use crate::{tracy_span, Variables};
use rust_decimal::prelude::*;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CalcResult {
    pub typ: CalcResultType,
    index_into_tokens: usize,
    index2_into_tokens: Option<usize>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CalcResultType {
    Number(Decimal),
    Percentage(Decimal),
    Unit(UnitOutput),
    Quantity(Decimal, UnitOutput),
    Matrix(MatrixData),
}

impl CalcResult {
    pub fn new(typ: CalcResultType, index: usize) -> CalcResult {
        CalcResult {
            typ,
            index_into_tokens: index,
            index2_into_tokens: None,
        }
    }

    pub fn new2(typ: CalcResultType, index: usize, index2: usize) -> CalcResult {
        CalcResult {
            typ,
            index_into_tokens: index,
            index2_into_tokens: Some(index2),
        }
    }

    pub fn get_index_into_tokens(&self) -> usize {
        self.index_into_tokens
    }

    pub fn set_token_error_flag<'text_ptr>(&self, tokens: &mut [Token<'text_ptr>]) {
        // TODO I could not reproduce it but it happened runtime, so I use 'get_mut'
        // later when those indices will be used correctly (now they are just dummy values lot of times),
        // we can use direct indexing
        Token::set_token_error_flag_by_index(self.index_into_tokens, tokens);
        if let Some(i2) = self.index2_into_tokens {
            Token::set_token_error_flag_by_index(i2, tokens);
        }
    }

    /// creates a cheap CalcResult without memory allocation. Use it only as a temporary value.
    pub fn hack_empty() -> CalcResult {
        CalcResult {
            typ: CalcResultType::Matrix(MatrixData {
                cells: Vec::new(),
                row_count: 0,
                col_count: 0,
            }),
            index_into_tokens: 0,
            index2_into_tokens: None,
        }
    }

    pub fn zero() -> CalcResult {
        CalcResult::new(CalcResultType::Number(Decimal::zero()), 0)
    }
}

pub struct EvaluationResult {
    pub there_was_unit_conversion: bool,
    pub there_was_operation: bool,
    pub assignment: bool,
    pub result: CalcResult,
}

#[derive(Debug, Clone)]
pub struct ShuntingYardResult {
    pub typ: TokenType,
    pub index_into_tokens: usize,
}

impl ShuntingYardResult {
    pub fn new(typ: TokenType, index_into_tokens: usize) -> ShuntingYardResult {
        ShuntingYardResult {
            typ,
            index_into_tokens,
        }
    }
}

pub fn evaluate_tokens<'text_ptr>(
    tokens: &mut [Token<'text_ptr>],
    shunting_tokens: &mut Vec<ShuntingYardResult>,
    variables: &Variables,
    units: &Units,
) -> Result<Option<EvaluationResult>, ()> {
    let _span = tracy_span("calc", file!(), line!());
    let mut stack: Vec<CalcResult> = vec![];
    let mut there_was_unit_conversion = false;
    let mut assignment = false;
    let mut last_success_operation_result_index = None;

    for i in 0..shunting_tokens.len() {
        let token = &shunting_tokens[i];
        match &token.typ {
            TokenType::NumberLiteral(num) => stack.push(CalcResult::new(
                CalcResultType::Number(num.clone()),
                token.index_into_tokens,
            )),
            TokenType::NumberErr => {
                return Err(());
            }
            TokenType::Unit(unit_typ, target_unit) => {
                // next token must be a UnitConverter or Div
                match unit_typ {
                    UnitTokenType::ApplyToPrevToken => {
                        let operand = stack.last();
                        if let Some(CalcResult {
                            typ: CalcResultType::Number(operand_num),
                            index_into_tokens,
                            index2_into_tokens: _index2_into_tokens,
                        }) = operand
                        {
                            if let Some(result) = unit_conversion(
                                operand_num,
                                target_unit,
                                *index_into_tokens,
                                token.index_into_tokens,
                            ) {
                                stack.pop();
                                stack.push(result);
                                last_success_operation_result_index = Some(stack.len() - 1);
                            } else {
                                Token::set_token_error_flag_by_index(
                                    token.index_into_tokens,
                                    tokens,
                                );
                                return Err(());
                            }
                        } else {
                            Token::set_token_error_flag_by_index(token.index_into_tokens, tokens);
                            return Err(());
                        }
                    }
                    UnitTokenType::StandInItself => {
                        if shunting_tokens
                            .get(i + 1)
                            .map(|it| {
                                matches!(
                                    it.typ,
                                    TokenType::Operator(OperatorTokenType::UnitConverter)
                                        | TokenType::Operator(OperatorTokenType::Div)
                                )
                            })
                            .unwrap_or(false)
                        {
                            // TODO clone
                            stack.push(CalcResult::new(
                                CalcResultType::Unit(target_unit.clone()),
                                token.index_into_tokens,
                            ));
                        } else {
                            dbg!(&tokens);
                            tokens[token.index_into_tokens].typ = TokenType::StringLiteral;
                        }
                    }
                }
            }
            TokenType::Operator(typ) => {
                if *typ == OperatorTokenType::Assign {
                    assignment = true;
                    continue;
                }
                if apply_operation(tokens, &mut stack, &typ, token.index_into_tokens, units) == true
                {
                    if matches!(typ, OperatorTokenType::UnitConverter) {
                        there_was_unit_conversion = true;
                    }
                    if !stack.is_empty() {
                        last_success_operation_result_index = Some(stack.len() - 1);
                    }
                } else {
                    return Err(());
                }
            }
            TokenType::StringLiteral | TokenType::Header => panic!(),
            TokenType::Variable { var_index } | TokenType::LineReference { var_index } => {
                // TODO clone :(
                match &variables[*var_index]
                    .as_ref()
                    .expect("var_index should be valid")
                    .value
                {
                    Ok(value) => {
                        stack.push(CalcResult::new(value.typ.clone(), token.index_into_tokens));
                    }
                    Err(_) => {
                        return Err(());
                    }
                }
            }
        }
    }
    return match last_success_operation_result_index {
        Some(last_success_operation_index) => {
            // e.g. "1+2 some text 3"
            // in this case prefer the result of 1+2 and convert 3 to String
            for (i, stack_elem) in stack.iter().enumerate() {
                if last_success_operation_index != i {
                    debug_print(&format!(
                        " calc> {:?} --> String",
                        &tokens[stack_elem.index_into_tokens]
                    ));
                    tokens[stack_elem.index_into_tokens].typ = TokenType::StringLiteral;
                }
            }
            Ok(Some(EvaluationResult {
                there_was_unit_conversion,
                there_was_operation: true,
                assignment,
                result: stack[last_success_operation_index].clone(),
            }))
        }
        None => Ok(stack.pop().map(|it| EvaluationResult {
            there_was_operation: false,
            there_was_unit_conversion,
            assignment,
            result: it,
        })),
    };
}

fn apply_operation<'text_ptr>(
    tokens: &mut [Token<'text_ptr>],
    stack: &mut Vec<CalcResult>,
    op: &OperatorTokenType,
    op_token_index: usize,
    units: &Units,
) -> bool {
    let succeed = match &op {
        OperatorTokenType::Mult
        | OperatorTokenType::Div
        | OperatorTokenType::Add
        | OperatorTokenType::Sub
        | OperatorTokenType::BinAnd
        | OperatorTokenType::BinOr
        | OperatorTokenType::BinXor
        | OperatorTokenType::Pow
        | OperatorTokenType::ShiftLeft
        | OperatorTokenType::ShiftRight
        | OperatorTokenType::Percentage_Find_Base_From_Result_Increase_X
        | OperatorTokenType::Percentage_Find_Base_From_X_Icrease_Result
        | OperatorTokenType::Percentage_Find_Base_From_Icrease_X_Result
        | OperatorTokenType::Percentage_Find_Incr_Rate_From_Result_X_Base
        | OperatorTokenType::Percentage_Find_Base_From_Result_Decrease_X
        | OperatorTokenType::Percentage_Find_Base_From_X_Decrease_Result
        | OperatorTokenType::Percentage_Find_Base_From_Decrease_X_Result
        | OperatorTokenType::Percentage_Find_Decr_Rate_From_Result_X_Base
        | OperatorTokenType::Percentage_Find_Rate_From_Result_Base
        | OperatorTokenType::Percentage_Find_Base_From_Result_Rate
        | OperatorTokenType::UnitConverter => {
            if stack.len() > 1 {
                let (lhs, rhs) = (&stack[stack.len() - 2], &stack[stack.len() - 1]);
                if let Some(result) = binary_operation(op, lhs, rhs) {
                    stack.truncate(stack.len() - 2);
                    stack.push(result);
                    true
                } else {
                    lhs.set_token_error_flag(tokens);
                    rhs.set_token_error_flag(tokens);
                    Token::set_token_error_flag_by_index(op_token_index, tokens);
                    false
                }
            } else {
                false
            }
        }
        OperatorTokenType::UnaryMinus
        | OperatorTokenType::UnaryPlus
        | OperatorTokenType::Perc
        | OperatorTokenType::BinNot => {
            let maybe_top = stack.last();
            let result = maybe_top.and_then(|top| unary_operation(&op, top, op_token_index));
            debug_print(&format!(
                "calc> {:?} {:?} = {:?}",
                &op,
                &maybe_top.as_ref().map(|it| &it.typ),
                &result
            ));
            if let Some(result) = result {
                stack.pop();
                stack.push(result);
                true
            } else {
                false
            }
        }
        OperatorTokenType::Matrix {
            row_count,
            col_count,
        } => {
            let arg_count = row_count * col_count;
            if stack.len() >= arg_count {
                let matrix_args = stack.drain(stack.len() - arg_count..).collect::<Vec<_>>();
                stack.push(CalcResult::new(
                    CalcResultType::Matrix(MatrixData::new(matrix_args, *row_count, *col_count)),
                    op_token_index,
                ));
                debug_print("calc> Matrix");
                true
            } else {
                debug_print("calc> Matrix");
                false
            }
        }
        OperatorTokenType::Fn { arg_count, typ } => {
            debug_print(&format!("calc> Fn {:?}", typ));
            typ.execute(*arg_count, stack, op_token_index, tokens, units)
        }
        OperatorTokenType::Semicolon | OperatorTokenType::Comma => {
            // ignore
            true
        }
        OperatorTokenType::Assign => panic!("handled in the main loop above"),
        OperatorTokenType::ParenOpen
        | OperatorTokenType::ParenClose
        | OperatorTokenType::BracketOpen
        | OperatorTokenType::BracketClose => {
            // this branch was executed during fuzz testing, don't panic here
            // check test_panic_fuzz_3
            return false;
        }
        OperatorTokenType::PercentageIs => {
            // ignore
            true
        }
    };
    return succeed;
}

fn unit_conversion<'text_ptr>(
    num: &Decimal,
    target_unit: &UnitOutput,
    operand_token_index: usize,
    unit_token_index: usize,
) -> Option<CalcResult> {
    let norm = target_unit.normalize(num);
    if target_unit.dimensions == EMPTY_UNIT_DIMENSIONS {
        // the units cancelled each other, e.g. km/m
        norm.map(|norm| CalcResult::new(CalcResultType::Number(norm), operand_token_index))
    } else {
        norm.map(|norm| {
            CalcResult::new2(
                CalcResultType::Quantity(norm, target_unit.clone()),
                operand_token_index,
                unit_token_index,
            )
        })
    }
}

fn unary_operation(
    op: &OperatorTokenType,
    top: &CalcResult,
    op_token_index: usize,
) -> Option<CalcResult> {
    return match &op {
        OperatorTokenType::UnaryPlus => Some(top.clone()),
        OperatorTokenType::UnaryMinus => unary_minus_op(top),
        OperatorTokenType::Perc => percentage_operator(top, op_token_index),
        OperatorTokenType::BinNot => bitwise_not(top),
        _ => None,
    };
}

fn binary_operation(
    op: &OperatorTokenType,
    lhs: &CalcResult,
    rhs: &CalcResult,
) -> Option<CalcResult> {
    let result = match &op {
        OperatorTokenType::Mult => multiply_op(lhs, rhs),
        OperatorTokenType::Div => divide_op(lhs, rhs),
        OperatorTokenType::Add => add_op(lhs, rhs),
        OperatorTokenType::Sub => sub_op(lhs, rhs),
        OperatorTokenType::BinAnd => bitwise_and_op(lhs, rhs),
        OperatorTokenType::BinOr => bitwise_or_op(lhs, rhs),
        OperatorTokenType::BinXor => bitwise_xor_op(lhs, rhs),
        OperatorTokenType::Pow => pow_op(lhs, rhs),
        OperatorTokenType::ShiftLeft => bitwise_shift_left(lhs, rhs),
        OperatorTokenType::ShiftRight => bitwise_shift_right(lhs, rhs),
        OperatorTokenType::Percentage_Find_Base_From_Result_Increase_X => {
            perc_num_is_xperc_on_what(lhs, rhs)
        }
        OperatorTokenType::Percentage_Find_Base_From_X_Icrease_Result => {
            perc_num_is_xperc_on_what(rhs, lhs)
        }
        OperatorTokenType::Percentage_Find_Base_From_Icrease_X_Result => {
            perc_num_is_xperc_on_what(rhs, lhs)
        }
        OperatorTokenType::Percentage_Find_Incr_Rate_From_Result_X_Base => {
            perc_num_is_what_perc_on_num(lhs, rhs)
        }
        //
        OperatorTokenType::Percentage_Find_Base_From_Result_Decrease_X => {
            perc_num_is_xperc_off_what(lhs, rhs)
        }
        OperatorTokenType::Percentage_Find_Base_From_X_Decrease_Result => {
            perc_num_is_xperc_off_what(rhs, lhs)
        }
        OperatorTokenType::Percentage_Find_Base_From_Decrease_X_Result => {
            perc_num_is_xperc_off_what(rhs, lhs)
        }
        OperatorTokenType::Percentage_Find_Decr_Rate_From_Result_X_Base => {
            perc_num_is_what_perc_off_num(lhs, rhs)
        }
        OperatorTokenType::Percentage_Find_Rate_From_Result_Base => {
            percentage_find_rate_from_result_base(lhs, rhs)
        }
        OperatorTokenType::Percentage_Find_Base_From_Result_Rate => {
            percentage_find_base_from_result_rate(lhs, rhs)
        }
        OperatorTokenType::UnitConverter => {
            return match (&lhs.typ, &rhs.typ) {
                (
                    CalcResultType::Quantity(lhs_num, source_unit),
                    CalcResultType::Unit(target_unit),
                ) => {
                    if source_unit == target_unit {
                        Some(CalcResult::new(
                            CalcResultType::Quantity(lhs_num.clone(), target_unit.clone()),
                            0,
                        ))
                    } else {
                        None
                    }
                }
                (CalcResultType::Matrix(mat), CalcResultType::Unit(..)) => {
                    let cells: Option<Vec<CalcResult>> = mat
                        .cells
                        .iter()
                        .map(|cell| binary_operation(op, cell, rhs))
                        .collect();
                    cells.map(|it| {
                        CalcResult::new(
                            CalcResultType::Matrix(MatrixData::new(
                                it,
                                mat.row_count,
                                mat.col_count,
                            )),
                            0,
                        )
                    })
                }
                _ => None,
            };
        }
        // todo: ronda h nem a tipusokkal kezelem le hanem panickal a többit
        // , csinálj egy TokenType::BinaryOp::Add
        _ => panic!(),
    };
    debug_print(&format!(
        "calc> {:?} {:?} {:?} = {:?}",
        &lhs.typ, op, &rhs.typ, &result
    ));
    result
}

fn percentage_operator(lhs: &CalcResult, op_token_index: usize) -> Option<CalcResult> {
    match &lhs.typ {
        CalcResultType::Number(lhs_num) => {
            // 5%
            Some(CalcResult::new2(
                CalcResultType::Percentage(lhs_num.clone()),
                lhs.index_into_tokens,
                op_token_index,
            ))
        }
        _ => None,
    }
}

fn bitwise_not(lhs: &CalcResult) -> Option<CalcResult> {
    match &lhs.typ {
        CalcResultType::Number(lhs_num) => {
            // 0b01 and 0b10
            let lhs_num = lhs_num.to_u64()?;
            Some(CalcResult::new(
                CalcResultType::Number(dec(lhs_num.not())),
                lhs.index_into_tokens,
            ))
        }
        _ => None,
    }
}

fn bitwise_xor_op(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    match (&lhs.typ, &rhs.typ) {
        //////////////
        // 12 and x
        //////////////
        (CalcResultType::Number(lhs), CalcResultType::Number(rhs)) => {
            // 0b01 and 0b10
            let lhs = lhs.to_u64()?;
            let rhs = rhs.to_u64()?;
            Some(CalcResult::new(
                CalcResultType::Number(dec(lhs.bitxor(rhs))),
                0,
            ))
        }
        _ => None,
    }
}

fn bitwise_or_op(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    match (&lhs.typ, &rhs.typ) {
        //////////////
        // 12 and x
        //////////////
        (CalcResultType::Number(lhs), CalcResultType::Number(rhs)) => {
            // 0b01 and 0b10
            let lhs = lhs.to_u64()?;
            let rhs = rhs.to_u64()?;
            Some(CalcResult::new(CalcResultType::Number(dec(lhs | rhs)), 0))
        }
        _ => None,
    }
}

fn perc_num_is_xperc_on_what(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    // 'lhs' is 'rhs' on what
    // 41 is 17% on what
    match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Number(y), CalcResultType::Percentage(p)) => {
            let x = y
                .checked_mul(&DECIMAL_100)?
                .checked_div(&p.checked_add(&DECIMAL_100)?)?;
            Some(CalcResult::new(CalcResultType::Number(x), 0))
        }
        _ => None,
    }
}

fn perc_num_is_xperc_off_what(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    // 'lhs' is 'rhs' off what
    // 41 is 17% off what
    // x = (y*100)/(100-p)
    match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Number(y), CalcResultType::Percentage(p)) => {
            let x = y
                .checked_mul(&DECIMAL_100)?
                .checked_div(&DECIMAL_100.checked_sub(&p)?)?;
            Some(CalcResult::new(CalcResultType::Number(x), 0))
        }
        _ => None,
    }
}

fn perc_num_is_what_perc_on_num(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    // lhs is what % on rhs
    // 41 is what % on 35
    match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Number(y), CalcResultType::Number(x)) => {
            let p = y
                .checked_mul(&DECIMAL_100)?
                .checked_div(x)?
                .checked_sub(&DECIMAL_100)?;
            Some(CalcResult::new(CalcResultType::Percentage(p), 0))
        }
        _ => None,
    }
}

fn perc_num_is_what_perc_off_num(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    // lhs is what % off rhs
    // 35 is what % off 41
    match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Number(y), CalcResultType::Number(x)) => {
            let p = y
                .checked_mul(&DECIMAL_100)?
                .checked_div(x)?
                .checked_sub(&DECIMAL_100)?
                .neg();
            Some(CalcResult::new(CalcResultType::Percentage(p), 0))
        }
        _ => None,
    }
}

fn percentage_find_rate_from_result_base(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    // lhs is what percent of lhs
    // 20 is what percent of 60
    match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Number(y), CalcResultType::Number(x)) => {
            let p = y.checked_div(x)?.checked_mul(&DECIMAL_100)?;
            Some(CalcResult::new(CalcResultType::Percentage(p), 0))
        }
        _ => None,
    }
}

fn percentage_find_base_from_result_rate(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    // lhs is rhs% of what
    // 5 is 25% of what
    match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Number(y), CalcResultType::Percentage(p)) => {
            let x = y.checked_div(p)?.checked_mul(&DECIMAL_100)?;
            Some(CalcResult::new(CalcResultType::Number(x), 0))
        }
        _ => None,
    }
}

fn bitwise_shift_right(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Number(lhs), CalcResultType::Number(rhs)) => {
            let lhs = lhs.to_u64()?;
            let rhs = rhs.to_u32()?;
            if rhs > 63 {
                None
            } else {
                Some(CalcResult::new(
                    CalcResultType::Number(dec(lhs.shr(rhs))),
                    0,
                ))
            }
        }
        _ => None,
    }
}

fn bitwise_shift_left(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Number(lhs), CalcResultType::Number(rhs)) => {
            let lhs = lhs.to_u64()?;
            let rhs = rhs.to_u32()?;
            if rhs > 63 {
                None
            } else {
                Some(CalcResult::new(
                    CalcResultType::Number(dec(lhs.shl(rhs))),
                    0,
                ))
            }
        }
        _ => None,
    }
}

fn bitwise_and_op(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    match (&lhs.typ, &rhs.typ) {
        //////////////
        // 12 and x
        //////////////
        (CalcResultType::Number(lhs), CalcResultType::Number(rhs)) => {
            // 0b01 and 0b10
            let lhs = lhs.to_u64()?;
            let rhs = rhs.to_u64()?;
            Some(CalcResult::new(CalcResultType::Number(dec(lhs & rhs)), 0))
        }
        _ => None,
    }
}

fn unary_minus_op(lhs: &CalcResult) -> Option<CalcResult> {
    match &lhs.typ {
        CalcResultType::Number(lhs_num) => {
            // -12
            Some(CalcResult::new(
                CalcResultType::Number(lhs_num.neg()),
                lhs.index_into_tokens,
            ))
        }
        CalcResultType::Quantity(lhs_num, unit) => {
            // -12km
            Some(CalcResult::new(
                CalcResultType::Quantity(lhs_num.neg(), unit.clone()),
                lhs.index_into_tokens,
            ))
        }
        CalcResultType::Percentage(lhs_num) => {
            // -50%
            Some(CalcResult::new(
                CalcResultType::Percentage(lhs_num.neg()),
                lhs.index_into_tokens,
            ))
        }
        _ => None, // CalcResultType::Matrix(mat) => CalcResultType::Matrix(mat.neg()),
    }
}

fn pow_op(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    match (&lhs.typ, &rhs.typ) {
        //////////////
        // 1^x
        //////////////
        (CalcResultType::Number(lhs), CalcResultType::Number(rhs)) => {
            // 2^3
            rhs.to_i64()
                .and_then(|rhs| {
                    let p = pow(lhs.clone(), rhs);
                    p
                })
                .map(|pow| CalcResult::new(CalcResultType::Number(pow), 0))
        }
        (CalcResultType::Quantity(lhs, lhs_unit), CalcResultType::Number(rhs)) => {
            let p = rhs.to_i64()?;
            let num_powered = pow(lhs.clone(), p)?;
            let unit_powered = lhs_unit.pow(p);
            dbg!(&p);
            dbg!(&num_powered);
            dbg!(&unit_powered);
            Some(CalcResult::new(
                CalcResultType::Quantity(num_powered, unit_powered?),
                0,
            ))
        }
        _ => None,
    }
}

pub fn multiply_op(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    let result = match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Unit(..), CalcResultType::Unit(..))
        | (CalcResultType::Unit(..), CalcResultType::Number(..))
        | (CalcResultType::Unit(..), CalcResultType::Quantity(..))
        | (CalcResultType::Unit(..), CalcResultType::Percentage(..))
        | (CalcResultType::Unit(..), CalcResultType::Matrix(..))
        | (CalcResultType::Number(..), CalcResultType::Unit(..))
        | (CalcResultType::Quantity(..), CalcResultType::Unit(..))
        | (CalcResultType::Percentage(..), CalcResultType::Unit(..))
        | (CalcResultType::Matrix(..), CalcResultType::Unit(..)) => None,
        //////////////
        // 12 * x
        //////////////
        (CalcResultType::Number(lhs), CalcResultType::Number(rhs)) => {
            // 12 * 2
            lhs.checked_mul(rhs)
                .map(|num| CalcResult::new(CalcResultType::Number(num), 0))
        }
        (CalcResultType::Number(lhs), CalcResultType::Quantity(rhs, unit)) => {
            // 12 * 2km
            lhs.checked_mul(rhs)
                .map(|num| CalcResult::new(CalcResultType::Quantity(num, unit.clone()), 0))
        }
        (CalcResultType::Number(lhs), CalcResultType::Percentage(rhs)) => {
            // 100 * 50%
            Some(CalcResult::new(
                CalcResultType::Number(percentage_of(rhs, lhs)?),
                0,
            ))
        }
        (CalcResultType::Number(..), CalcResultType::Matrix(mat)) => mat.mult_scalar(lhs),
        //////////////
        // 12km * x
        //////////////
        (CalcResultType::Quantity(lhs_num, lhs_unit), CalcResultType::Number(rhs_num)) => {
            // 2m * 5
            lhs_num
                .checked_mul(rhs_num)
                .map(|num| CalcResult::new(CalcResultType::Quantity(num, lhs_unit.clone()), 0))
        }
        (
            CalcResultType::Quantity(lhs_num, lhs_unit),
            CalcResultType::Quantity(rhs_num, rhs_unit),
        ) => {
            // 2s * 3s
            if lhs_unit.unit_count + rhs_unit.unit_count >= MAX_UNIT_COUNT {
                None
            } else {
                let new_unit = lhs_unit * rhs_unit;
                if new_unit.is_unitless() {
                    lhs_num
                        .checked_mul(&rhs_num)
                        .map(|num| CalcResult::new(CalcResultType::Number(num), 0))
                } else {
                    lhs_num
                        .checked_mul(rhs_num)
                        .map(|num| CalcResult::new(CalcResultType::Quantity(num, new_unit), 0))
                }
            }
        }
        (CalcResultType::Quantity(lhs, lhs_unit), CalcResultType::Percentage(rhs)) => {
            // e.g. 2m * 50%
            Some(CalcResult::new(
                CalcResultType::Quantity(percentage_of(rhs, lhs)?, lhs_unit.clone()),
                0,
            ))
        }
        (CalcResultType::Quantity(..), CalcResultType::Matrix(mat)) => mat.mult_scalar(lhs),
        //////////////
        // 12% * x
        //////////////
        (CalcResultType::Percentage(lhs), CalcResultType::Number(rhs)) => {
            // 5% * 10
            Some(CalcResult::new(
                CalcResultType::Number(percentage_of(lhs, rhs)?),
                0,
            ))
        }
        (CalcResultType::Percentage(lhs), CalcResultType::Quantity(rhs, rhs_unit)) => {
            // 5% * 10km
            Some(CalcResult::new(
                CalcResultType::Quantity(percentage_of(lhs, rhs)?, rhs_unit.clone()),
                0,
            ))
        }
        (CalcResultType::Percentage(lhs), CalcResultType::Percentage(rhs)) => {
            // 50% * 50%

            Some(CalcResult::new(
                CalcResultType::Percentage(
                    (lhs.checked_div(&DECIMAL_100)?)
                        .checked_mul(&rhs.checked_div(&DECIMAL_100)?)?,
                ),
                0,
            ))
        }
        (CalcResultType::Percentage(..), CalcResultType::Matrix(..)) => None,
        //////////////
        // Matrix
        //////////////
        (CalcResultType::Matrix(mat), CalcResultType::Number(..))
        | (CalcResultType::Matrix(mat), CalcResultType::Quantity(..))
        | (CalcResultType::Matrix(mat), CalcResultType::Percentage(..)) => mat.mult_scalar(rhs),
        (CalcResultType::Matrix(a), CalcResultType::Matrix(b)) => {
            if a.col_count != b.row_count {
                return None;
            }
            let mut result = Vec::with_capacity(a.row_count * b.col_count);
            for row in 0..a.row_count {
                for col in 0..b.col_count {
                    let mut sum = if let Some(r) = multiply_op(a.cell(row, 0), b.cell(0, col)) {
                        r
                    } else {
                        return None;
                    };
                    for i in 1..a.col_count {
                        if let Some(r) = multiply_op(a.cell(row, i), b.cell(i, col)) {
                            if let Some(s) = add_op(&sum, &r) {
                                sum = s;
                            } else {
                                return None;
                            }
                        }
                    }
                    result.push(sum);
                }
            }
            Some(CalcResult::new(
                CalcResultType::Matrix(MatrixData::new(result, a.row_count, b.col_count)),
                0,
            ))
        }
    };
    return match result {
        Some(CalcResult {
            typ: CalcResultType::Quantity(num, unit),
            ..
        }) if unit.is_unitless() => {
            // some operation cancelled out its units, put a simple number on the stack
            Some(CalcResult::new(CalcResultType::Number(num), 0))
        }
        _ => result,
    };
}

pub fn add_op(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Unit(..), CalcResultType::Unit(..))
        | (CalcResultType::Unit(..), CalcResultType::Number(..))
        | (CalcResultType::Unit(..), CalcResultType::Quantity(..))
        | (CalcResultType::Unit(..), CalcResultType::Percentage(..))
        | (CalcResultType::Unit(..), CalcResultType::Matrix(..))
        | (CalcResultType::Number(..), CalcResultType::Unit(..))
        | (CalcResultType::Quantity(..), CalcResultType::Unit(..))
        | (CalcResultType::Percentage(..), CalcResultType::Unit(..))
        | (CalcResultType::Matrix(..), CalcResultType::Unit(..)) => None,
        //////////////
        // 12 + x
        //////////////
        (CalcResultType::Number(lhs), CalcResultType::Number(rhs)) => {
            // 12 + 3
            Some(CalcResult::new(
                CalcResultType::Number(lhs.checked_add(&rhs)?),
                0,
            ))
        }
        (CalcResultType::Number(_lhs), CalcResultType::Quantity(_rhs, _unit)) => {
            // 12 + 3 km
            None
        }
        (CalcResultType::Number(lhs), CalcResultType::Percentage(rhs)) => {
            // 100 + 50%
            let x_percent_of_left_hand_side = lhs
                .checked_div(&DECIMAL_100)
                .and_then(|it| it.checked_mul(rhs))?;
            Some(CalcResult::new(
                CalcResultType::Number(lhs.checked_add(&x_percent_of_left_hand_side)?),
                0,
            ))
        }
        (CalcResultType::Number(_lhs), CalcResultType::Matrix(..)) => None,
        //////////////
        // 12km + x
        //////////////
        (CalcResultType::Quantity(_lhs, _lhs_unit), CalcResultType::Number(_rhs)) => {
            // 2m + 5
            None
        }
        (CalcResultType::Quantity(lhs, lhs_unit), CalcResultType::Quantity(rhs, rhs_unit)) => {
            // 2s + 3s
            if lhs_unit != rhs_unit {
                None
            } else {
                Some(CalcResult::new(
                    CalcResultType::Quantity(lhs.checked_add(rhs)?, lhs_unit.clone()),
                    0,
                ))
            }
        }
        (CalcResultType::Quantity(lhs, lhs_unit), CalcResultType::Percentage(rhs)) => {
            // e.g. 2m + 50%
            let x_percent_of_left_hand_side = lhs
                .checked_div(&DECIMAL_100)
                .and_then(|it| it.checked_mul(rhs))?;
            Some(CalcResult::new(
                CalcResultType::Quantity(lhs + x_percent_of_left_hand_side, lhs_unit.clone()),
                0,
            ))
        }
        (CalcResultType::Quantity(..), CalcResultType::Matrix(..)) => None,
        //////////////
        // 12% + x
        //////////////
        (CalcResultType::Percentage(_lhs), CalcResultType::Number(_rhs)) => {
            // 5% + 10
            None
        }
        (CalcResultType::Percentage(_lhs), CalcResultType::Quantity(_rhs, _rhs_unit)) => {
            // 5% + 10km
            None
        }
        (CalcResultType::Percentage(lhs), CalcResultType::Percentage(rhs)) => {
            // 50% + 50%
            Some(CalcResult::new(CalcResultType::Percentage(lhs + rhs), 0))
        }
        (CalcResultType::Percentage(..), CalcResultType::Matrix(..)) => None,
        ///////////
        // Matrix
        //////////
        (CalcResultType::Matrix(..), CalcResultType::Number(..)) => None,
        (CalcResultType::Matrix(..), CalcResultType::Quantity(..)) => None,
        (CalcResultType::Matrix(..), CalcResultType::Percentage(..)) => None,
        (CalcResultType::Matrix(lhs), CalcResultType::Matrix(rhs)) => {
            if lhs.row_count != rhs.row_count || lhs.col_count != rhs.col_count {
                return None;
            }
            let cells: Option<Vec<CalcResult>> = lhs
                .cells
                .iter()
                .zip(rhs.cells.iter())
                .map(|(a, b)| add_op(a, b))
                .collect();
            cells.map(|it| {
                CalcResult::new(
                    CalcResultType::Matrix(MatrixData::new(it, lhs.row_count, lhs.col_count)),
                    0,
                )
            })
        }
    }
}

fn sub_op(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Unit(..), CalcResultType::Unit(..))
        | (CalcResultType::Unit(..), CalcResultType::Number(..))
        | (CalcResultType::Unit(..), CalcResultType::Quantity(..))
        | (CalcResultType::Unit(..), CalcResultType::Percentage(..))
        | (CalcResultType::Unit(..), CalcResultType::Matrix(..))
        | (CalcResultType::Number(..), CalcResultType::Unit(..))
        | (CalcResultType::Quantity(..), CalcResultType::Unit(..))
        | (CalcResultType::Percentage(..), CalcResultType::Unit(..))
        | (CalcResultType::Matrix(..), CalcResultType::Unit(..)) => None,
        //////////////
        // 12 - x
        //////////////
        (CalcResultType::Number(lhs), CalcResultType::Number(rhs)) => {
            // 12 - 3
            Some(CalcResult::new(
                CalcResultType::Number(lhs.checked_sub(&rhs)?),
                0,
            ))
        }
        (CalcResultType::Number(_lhs), CalcResultType::Quantity(_rhs, _unit)) => {
            // 12 - 3 km
            None
        }
        (CalcResultType::Number(lhs), CalcResultType::Percentage(rhs)) => {
            // 100 - 50%
            let x_percent_of_left_hand_side = lhs
                .checked_div(&DECIMAL_100)
                .and_then(|it| it.checked_mul(rhs))?;
            Some(CalcResult::new(
                CalcResultType::Number(lhs.checked_sub(&x_percent_of_left_hand_side)?),
                0,
            ))
        }
        (CalcResultType::Number(..), CalcResultType::Matrix(..)) => None,
        //////////////
        // 12km - x
        //////////////
        (CalcResultType::Quantity(_lhs, _lhs_unit), CalcResultType::Number(_rhs)) => {
            // 2m - 5
            None
        }
        (CalcResultType::Quantity(lhs, lhs_unit), CalcResultType::Quantity(rhs, rhs_unit)) => {
            // 2s - 3s
            if lhs_unit != rhs_unit {
                None
            } else {
                Some(CalcResult::new(
                    CalcResultType::Quantity(lhs.checked_sub(rhs)?, lhs_unit.clone()),
                    0,
                ))
            }
        }
        (CalcResultType::Quantity(lhs, lhs_unit), CalcResultType::Percentage(rhs)) => {
            // e.g. 2m - 50%
            let x_percent_of_left_hand_side = lhs
                .checked_div(&DECIMAL_100)
                .and_then(|it| it.checked_mul(rhs))?;
            Some(CalcResult::new(
                CalcResultType::Quantity(
                    lhs.checked_sub(&x_percent_of_left_hand_side)?,
                    lhs_unit.clone(),
                ),
                0,
            ))
        }
        (CalcResultType::Quantity(..), CalcResultType::Matrix(..)) => None,
        //////////////
        // 12% - x
        //////////////
        (CalcResultType::Percentage(_lhs), CalcResultType::Number(_rhs)) => {
            // 5% - 10
            None
        }
        (CalcResultType::Percentage(_lhs), CalcResultType::Quantity(_rhs, _rhs_unit)) => {
            // 5% - 10km
            None
        }
        (CalcResultType::Percentage(lhs), CalcResultType::Percentage(rhs)) => {
            // 50% - 50%
            Some(CalcResult::new(
                CalcResultType::Percentage(lhs.checked_sub(&rhs)?),
                0,
            ))
        }
        (CalcResultType::Percentage(..), CalcResultType::Matrix(..)) => None,
        ///////////
        // Matrix
        //////////
        (CalcResultType::Matrix(..), CalcResultType::Number(..)) => None,
        (CalcResultType::Matrix(..), CalcResultType::Quantity(..)) => None,
        (CalcResultType::Matrix(..), CalcResultType::Percentage(..)) => None,
        (CalcResultType::Matrix(lhs), CalcResultType::Matrix(rhs)) => {
            if lhs.row_count != rhs.row_count || lhs.col_count != rhs.col_count {
                return None;
            }
            let cells: Option<Vec<CalcResult>> = lhs
                .cells
                .iter()
                .zip(rhs.cells.iter())
                .map(|(a, b)| sub_op(a, b))
                .collect();
            cells.map(|it| {
                CalcResult::new(
                    CalcResultType::Matrix(MatrixData::new(it, lhs.row_count, lhs.col_count)),
                    0,
                )
            })
        }
    }
}

pub fn divide_op(lhs: &CalcResult, rhs: &CalcResult) -> Option<CalcResult> {
    let result: Option<CalcResult> = match (&lhs.typ, &rhs.typ) {
        (CalcResultType::Unit(..), CalcResultType::Unit(..))
        | (CalcResultType::Unit(..), CalcResultType::Number(..))
        | (CalcResultType::Unit(..), CalcResultType::Quantity(..))
        | (CalcResultType::Unit(..), CalcResultType::Percentage(..))
        | (CalcResultType::Unit(..), CalcResultType::Matrix(..))
        | (CalcResultType::Matrix(..), CalcResultType::Unit(..)) => None,
        //////////////
        // 12 / year
        //////////////
        (CalcResultType::Quantity(lhs_num, lhs_unit), CalcResultType::Unit(rhs_unit)) => {
            let new_unit = lhs_unit / rhs_unit;
            if new_unit.is_unitless() {
                if let Some(lhs_num) = lhs_unit.from_base_to_this_unit(lhs_num) {
                    Some(CalcResult::new(CalcResultType::Number(lhs_num), 0))
                } else {
                    None
                }
            } else {
                Some(CalcResult::new(
                    CalcResultType::Quantity(lhs_num.clone(), new_unit),
                    0,
                ))
            }
        }
        (CalcResultType::Number(num), CalcResultType::Unit(unit)) => {
            let new_unit = unit.pow(-1)?;
            let num_part = new_unit.normalize(&num)?;
            Some(CalcResult::new(
                CalcResultType::Quantity(num_part, new_unit),
                0,
            ))
        }
        //////////////
        // 5% / year
        //////////////
        (CalcResultType::Percentage(num), CalcResultType::Unit(unit)) => {
            let new_unit = unit.pow(-1)?;
            let num_part = new_unit.normalize(&num.checked_div(&DECIMAL_100)?)?;
            Some(CalcResult::new(
                CalcResultType::Quantity(num_part, new_unit),
                0,
            ))
        }
        //////////////
        // 12 / x
        //////////////
        (CalcResultType::Number(lhs), CalcResultType::Number(rhs)) => {
            // 100 / 2
            Some(CalcResult::new(
                CalcResultType::Number(lhs.checked_div(&rhs)?),
                0,
            ))
        }
        (CalcResultType::Number(lhs), CalcResultType::Quantity(rhs, unit)) => {
            // 100 / 2km => 100 / (2 km)
            let new_unit = unit.pow(-1)?;

            let denormalized_num = unit.from_base_to_this_unit(rhs)?;
            if denormalized_num.is_zero() {
                return None;
            }
            let num_part = new_unit.normalize(&(lhs / &denormalized_num))?;
            Some(CalcResult::new(
                CalcResultType::Quantity(num_part, new_unit.clone()),
                0,
            ))
        }
        (CalcResultType::Number(lhs), CalcResultType::Percentage(rhs)) => {
            if rhs.is_zero() {
                return None;
            }
            // 100 / 50%
            Some(CalcResult::new(
                CalcResultType::Percentage(lhs.checked_div(rhs)?.checked_mul(&DECIMAL_100)?),
                0,
            ))
        }
        (CalcResultType::Number(..), CalcResultType::Matrix(..)) => None,
        //////////////
        // 12km / x
        //////////////
        (CalcResultType::Quantity(lhs, lhs_unit), CalcResultType::Number(rhs)) => {
            // 2m / 5
            if rhs.is_zero() {
                return None;
            }
            Some(CalcResult::new(
                CalcResultType::Quantity(lhs / rhs, lhs_unit.clone()),
                0,
            ))
        }
        (CalcResultType::Quantity(lhs, lhs_unit), CalcResultType::Quantity(rhs, rhs_unit)) => {
            // 12 km / 3s
            if rhs.is_zero() {
                return None;
            } else if lhs_unit.unit_count + rhs_unit.unit_count >= MAX_UNIT_COUNT {
                None
            } else {
                Some(CalcResult::new(
                    CalcResultType::Quantity(lhs / rhs, lhs_unit / rhs_unit),
                    0,
                ))
            }
        }
        (CalcResultType::Quantity(_lhs, _lhs_unit), CalcResultType::Percentage(_rhs)) => {
            // 2m / 50%
            None
        }
        (CalcResultType::Quantity(..), CalcResultType::Matrix(..)) => None,
        //////////////
        // 12% / x
        //////////////
        (CalcResultType::Percentage(_lhs), CalcResultType::Number(_rhs)) => {
            // 5% / 10
            None
        }
        (CalcResultType::Percentage(_lhs), CalcResultType::Quantity(_rhs, _rhs_unit)) => {
            // 5% / 10km
            None
        }
        (CalcResultType::Percentage(_lhs), CalcResultType::Percentage(_rhs)) => {
            // 50% / 50%
            None
        }
        (CalcResultType::Percentage(..), CalcResultType::Matrix(..)) => None,
        (CalcResultType::Matrix(mat), CalcResultType::Number(..))
        | (CalcResultType::Matrix(mat), CalcResultType::Quantity(..))
        | (CalcResultType::Matrix(mat), CalcResultType::Percentage(..)) => mat.div_scalar(rhs),
        (CalcResultType::Matrix(..), CalcResultType::Matrix(..)) => None,
    };
    return match result {
        Some(CalcResult {
            typ: CalcResultType::Quantity(num, unit),
            ..
        }) if unit.is_unitless() => {
            // some operation cancelled out its units, put a simple number on the stack
            Some(CalcResult::new(CalcResultType::Number(num), 0))
        }
        _ => result,
    };
}

pub fn pow(this: Decimal, mut exp: i64) -> Option<Decimal> {
    if this.is_zero() && exp.is_negative() {
        return None;
    }
    let mut base = this.clone();
    let mut acc = Decimal::one();
    let neg = exp < 0;

    exp = exp.abs();

    while exp > 1 {
        if (exp & 1) == 1 {
            acc = acc.checked_mul(&base)?;
        }
        exp /= 2;
        // TODO improve square
        base = base.checked_mul(&base)?;
    }

    if exp == 1 {
        acc = acc.checked_mul(&base)?;
    }

    Some(if neg {
        Decimal::one().checked_div(&acc)?
    } else {
        acc
    })
}

pub fn dec<T: Into<Decimal>>(num: T) -> Decimal {
    num.into()
}

const DECIMAL_100: Decimal = Decimal::from_parts(100, 0, 0, false, 0);

fn percentage_of(this: &Decimal, base: &Decimal) -> Option<Decimal> {
    base.checked_div(&DECIMAL_100)?.checked_mul(this)
}

#[cfg(test)]
mod tests {
    use crate::shunting_yard::tests::{
        apply_to_prev_token_unit, apply_to_prev_token_unit_with_err, num, num_with_err, op, op_err,
        str, unit,
    };
    use crate::units::units::Units;
    use crate::{ResultFormat, Variable, Variables};
    use std::str::FromStr;

    use crate::borrow_checker_fighter::create_vars;
    use crate::calc::{CalcResult, CalcResultType, EvaluationResult};
    use crate::functions::FnType;
    use crate::renderer::render_result;
    use crate::token_parser::{OperatorTokenType, Token};
    use bumpalo::Bump;
    use rust_decimal::prelude::*;

    const DECIMAL_COUNT: usize = 4;

    fn test_tokens(text: &str, expected_tokens: &[Token]) {
        println!("===================================================");
        println!("{}", text);
        let units = Units::new();
        let temp = text.chars().collect::<Vec<char>>();
        let mut tokens = vec![];
        let vars = create_vars();
        let arena = Bump::new();
        let mut shunting_output = crate::shunting_yard::tests::do_shunting_yard_for_tests(
            &temp,
            &units,
            &mut tokens,
            &vars,
            &arena,
        );
        let _result_stack =
            crate::calc::evaluate_tokens(&mut tokens, &mut shunting_output, &vars, &units);

        crate::shunting_yard::tests::compare_tokens(text, &expected_tokens, &tokens);
    }

    fn test_vars(vars: &Variables, text: &str, expected: &str, dec_count: usize) {
        dbg!("===========================================================");
        dbg!(text);
        let temp = text.chars().collect::<Vec<char>>();

        let units = Units::new();

        let mut tokens = vec![];
        let arena = Bump::new();
        let mut shunting_output = crate::shunting_yard::tests::do_shunting_yard_for_tests(
            &temp,
            &units,
            &mut tokens,
            vars,
            &arena,
        );

        let result = crate::calc::evaluate_tokens(&mut tokens, &mut shunting_output, vars, &units);

        if let Err(..) = &result {
            assert_eq!("Err", expected);
        } else if let Ok(Some(EvaluationResult {
            there_was_unit_conversion,
            there_was_operation: _,
            assignment: _assignment,
            result:
                CalcResult {
                    typ: CalcResultType::Quantity(_num, _unit),
                    ..
                },
        })) = &result
        {
            assert_eq!(
                render_result(
                    &units,
                    &result.as_ref().unwrap().as_ref().unwrap().result,
                    &ResultFormat::Dec,
                    *there_was_unit_conversion,
                    Some(dec_count),
                    false,
                ),
                expected
            );
        } else if let Ok(..) = &result {
            assert_eq!(
                result
                    .unwrap()
                    .map(|it| render_result(
                        &units,
                        &it.result,
                        &ResultFormat::Dec,
                        false,
                        Some(dec_count),
                        false
                    ))
                    .unwrap_or(" ".to_string()),
                expected,
            );
        }
    }

    fn test(text: &str, expected: &str) {
        test_vars(&create_vars(), text, expected, DECIMAL_COUNT);
    }

    fn test_with_dec_count(dec_count: usize, text: &str, expected: &'static str) {
        test_vars(&create_vars(), text, expected, dec_count);
    }

    #[test]
    fn calc_tests() {
        test("2^-2", "0.25");
        test_with_dec_count(5, "5km + 5cm", "5.00005 km");
        test("5kg*m / 1s^2", "5 N");
        test("0.000001 km2 in m2", "1 m2");
        test("0.000000001 km3 in m3", "1 m3");

        test("0.000000002 km^3 in m^3", "2 m^3");
        test("0.000000002 km3 in m^3", "2 m^3");

        test("2 - -1", "3");

        test("24 bla + 0", "24");

        // should skip automatic simplification if created directly in the constructor
        test("9.81 kg*m/s^2 * 1", "9.81 N");

        // should test whether two units are equal
        test("100 cm in m", "1 m");
        test("5000 cm in m", "50 m");

        test("100 ft * lbf in (in*lbf)", "1200 in lbf");
        test("100 N in kg*m / s ^ 2", "100 (kg m) / s^2");
        test("100 cm in m", "1 m");
        test("100 Hz in 1/s", "100 / s");
        test("() Hz", " ");

        test("1 ft * lbf * 2 rad", "2 ft lbf rad");
        test("1 ft * lbf * 2 rad in in*lbf*rad", "24 in lbf rad");
        test("(2/3)m", "0.6667 m");
        test_with_dec_count(50, "(2/3)m", "0.6667 m");
        test_with_dec_count(50, "2/3m", "0.6667 / m");

        test("123 N in (kg m)/s^2", "123 (kg m) / s^2");

        test("1 km / 3000000 mm", "0.3333");
        test_with_dec_count(100, "1 km / 3000000 mm", "0.3333");

        test("5kg * 1", "5 kg");
        test("5 kg * 1", "5 kg");
        test(" 5 kg  * 1", "5 kg");
        test("-5kg  * 1", "-5 kg");
        test("+5kg  * 1", "5 kg");
        test(".5kg  * 1", "0.5 kg");
        test_with_dec_count(6, "-5mg in kg", "-0.000005 kg");
        test("5.2mg * 1", "5.2 mg");

        test("981 cm/s^2 in m/s^2", "9.81 m / s^2");
        test("5exabytes in bytes", "5000000000000000000 bytes");
        test(
            "8.314 kg*(m^2 / (s^2 / (K^-1 / mol))) * 1",
            "8.314 (kg m^2) / (s^2 K mol)",
        );

        test("9.81 meters/second^2 * 1", "9.81 meter / second^2");
        test("10 decades in decade", "10 decade");
        test("10 centuries in century", "10 century");
        test("10 millennia in millennium", "10 millennium");

        test("(10 + 20)km", "30 km");
    }

    #[test]
    fn calc_exp_test() {
        // exp, binary and hex does not work with units
        // test("5e3kg", "5000 kg");
        // test("3 kg^1.0e0 * m^1.0e0 * s^-2e0", "3 (kg m) / s^2");

        test_with_dec_count(5, "2.3e-4 + 0", "0.00023");
        test("2.8e-4 + 0", "0.0003");

        // TODO rust_decimal's range is too small for this :(
        test("1.23e50 + 0", "Err");
        // test(
        //     "1.23e50 + 0",
        //     "123000000000000000000000000000000000000000000000000",
        // );

        test("3 e + 0", "3");
        test("3e + 0", "3");
        test("33e + 0", "33");
        test("3e3 + 0", "3000");

        // it interprets it as 3 - (-3)
        test("3e--3", "6");

        // invalid input tests
        test("2.3e4e5 + 0", "23000");
    }

    #[test]
    fn test_percentages() {
        test("200 km/h * 10%", "20 km / h");
        test("200 km/h * 0%", "0 km / h");
        test("200 km/h + 10%", "220 km / h");
        test("200 km/h - 10%", "180 km / h");
        test("200 km/h + 0%", "200 km / h");
        test("200 km/h - 0%", "200 km / h");

        test("0 + 10%", "0");
        test("200 - 10%", "180");
        test("200 - 0%", "200");
        test("0 - 10%", "0");
        test("200 * 10%", "20");
        test("200 * 0%", "0");
        test("10% * 200", "20");
        test("0% * 200", "0");
        test("(10 + 20)%", "30 %");

        test("30/200%", "15 %");
    }

    #[test]
    fn test_longer_texts3() {
        test("I traveled 13km at a rate / 40km/h in min", "19.5 min");
    }

    #[test]
    fn test_longer_texts3_tokens() {
        test_tokens(
            "I traveled 13km at a rate / 40km/h in min",
            &[
                str("I"),
                str(" "),
                str("traveled"),
                str(" "),
                num(13),
                apply_to_prev_token_unit("km"),
                str(" "),
                str("at"),
                str(" "),
                str("a"),
                str(" "),
                str("rate"),
                str(" "),
                op(OperatorTokenType::Div),
                str(" "),
                num(40),
                apply_to_prev_token_unit("km / h"),
                str(" "),
                op(OperatorTokenType::UnitConverter),
                str(" "),
                unit("min"),
            ],
        );
    }

    #[test]
    fn test_longer_texts() {
        test(
            "I traveled 24 miles and rode my bike  / 2 hours",
            "12 mile / hour",
        );
        test(
            "Now let's say you rode your bike at a rate of 10 miles/h for * 4 h in mile",
            "40 mile",
        );
        test(
            "Now let's say you rode your bike at a rate of 10 miles/h for * 4 h",
            "64373.76 m",
        );
        test(
            " is a unit but should not be handled here so... 37.5MB*1 of DNA information in it.",
            "37.5 MB",
        );
    }

    #[test]
    fn test_longer_texts2() {
        test(
            "transfer of around 1.587GB in about / 3 seconds",
            "0.529 GB / second",
        );
    }

    #[test]
    fn test_result_heuristics() {
        // 2 numbers but no oepration, select none
        test("2.3e4.0e5", "23000");

        // ignore "15" and return with the last successful operation
        test("75-15 euróból kell adózni mert 15 EUR adómentes", "60");

        test("15 EUR adómentes azaz 75-15 euróból kell adózni", "60");
    }

    #[test]
    fn test_dont_count_zeroes() {
        test("1k * 1", "1000");
        test("2k * 1", "2000");
        test("3k - 2k", "1000");

        test("1k*1", "1000");
        test("2k*1", "2000");
        test("3k-2k", "1000");

        test("1M * 1", "1000000");
        test("2M * 1", "2000000");
        test("3M - 2M", "1000000");

        test("3M + 1k", "3001000");
        test("3M * 2k", "6000000000");
        // missing digit
        test("3M + k", "3000000");

        test("2kalap * 1", "2");
    }

    #[test]
    fn test_quant_vs_non_quant() {
        test("12 km/h * 5 ", "60 km / h");
        test("200kg alma + 300 kg banán ", "500 kg");

        test("3000/50ml", "60 / ml");
        test("(3000/50)ml", "60 ml");
        test("3000/(50ml)", "60 / ml");
        test("1/(2km/h)", "0.5 h / km");
    }

    #[test]
    fn tests_for_invalid_input() {
        test("3", "3");
        test("3e-3-", "0.003");

        test_tokens(
            "[2, asda]",
            &[
                str("["),
                str("2"),
                str(","),
                str(" "),
                str("asda"),
                str("]"),
            ],
        );
        test("[2, asda]", " ");

        test(
            "2+3 - this minus sign is part of the text, should not affect the result",
            "5",
        );

        test_tokens(
            "1szer sem jött el + *megjegyzés 2 éve...",
            &[
                num(1),
                str("szer"),
                str(" "),
                str("sem"),
                str(" "),
                str("jött"),
                str(" "),
                str("el"),
                str(" "),
                str("+"),
                str(" "),
                str("*"),
                str("megjegyzés"),
                str(" "),
                str("2"),
                str(" "),
                str("éve..."),
            ],
        );
        test("1szer sem jött el + *megjegyzés 2 éve...", "1");

        test("100 Hz in s", "Err");

        test("12m/h * 45s ^^", "0.15 m");
        test("12km/h * 45s ^^", "150 m");
        test_tokens(
            "12km/h * 45s ^^",
            &[
                num(12),
                apply_to_prev_token_unit("km / h"),
                str(" "),
                op(OperatorTokenType::Mult),
                str(" "),
                num(45),
                apply_to_prev_token_unit("s"),
                str(" "),
                str("^"),
                str("^"),
            ],
        );

        // there are no empty vectors

        // matrix
        test_tokens(
            "1 + [2,]",
            &[
                num(1),
                str(" "),
                str("+"),
                str(" "),
                str("["),
                str("2"),
                str(","),
                str("]"),
            ],
        );
        test("1 + [2,]", "1");

        // multiply operator must be explicit, "5" is ignored here
        test("5(1+2)", "3");

        // invalid
        test("[[2 * 1]]", "[2]");
        test("[[2 * 3, 4]]", "[6, 4]");
        test("[[2 * 1, 3], [4, 5]]", "[4, 5]");
    }

    #[test]
    fn calc_simplify_units() {
        // simplify from base to derived units if possible
        test("3 kg * m * 1 s^-2", "3 N");

        test("128PiB / 30Mb/s", "38430716586.6667 s");
        test_with_dec_count(39, "128PiB / 30Mb/s", "38430716586.6667 s");
        test_with_dec_count(40, "128PiB / 30Mb/s", "38430716586.6667 s");
    }

    #[test]
    fn unit_calcs() {
        test_with_dec_count(5, "50km + 50mm", "50.00005 km");
        test_with_dec_count(5, "50km - 50mm", "49.99995 km");
        test("5kg * 5g", "0.025 kg^2");
        test("5km * 5mm", "25 m^2");
    }

    #[test]
    fn test_calc_angles() {
        test("1 radian in rad", "1 rad");
        test_with_dec_count(51, "1 deg in rad", "0.0174532925199432957692369077 rad");
    }

    #[test]
    fn test_cancelling_out() {
        test("40 m * 40 N / 40 J", "40");
        test("3 (s^-1) * 4 s", "12");
        test("(8.314 J / mol / K) ^ 0", "1");
        test("60 minute / 1 s", "3600");
        test_with_dec_count(
            303,
            "60 km/h*h/h/h * 1",
            "0.0046296296296296296296296307 m / s^2",
        );
        // it is a very important test, if it gets converted wrongly
        // then 60 km/h is converted to m/s, which is 16.6666...7 m/s,
        // and it causes inaccuracies
        test("60km/h * 2h", "120000 m");
        test("60km/h * 2h in km", "120 km");
        test("1s * 2s^-1", "2");
        test("2s * 3(s^-1)", "6");
        test("2s * 3(1/s)", "6");
    }

    #[test]
    fn test_calc_inside_matrix() {
        test("[2 * 1]", "[2]");
        test("[2 * 1, 3]", "[2, 3]");
        test("[2 * 1, 3, 4, 5, 6]", "[2, 3, 4, 5, 6]");

        test("[2+3]", "[5]");
        test("[2+3, 4 - 1, 5*2, 6/3, 2^4]", "[5, 3, 10, 2, 16]");

        test("[2 * 1]", "[2]");
        test("[2 * 3; 4]", "[6; 4]");
        test("[2 * 1, 3; 4, 5]", "[2, 3; 4, 5]");
    }

    #[test]
    fn test_matrix_addition() {
        test("[2] + [3]", "[5]");
        test("[2, 3] + [4, 5]", "[6, 8]");
        test("[2, 3, 4] + [5, 6, 7]", "[7, 9, 11]");
        test("[2; 3] + [4; 5]", "[6; 8]");
        test(
            "[2, 3, 4; 5, 6, 7] + [8, 9, 10; 11, 12, 13]",
            "[10, 12, 14; 16, 18, 20]",
        );

        test("2 km + [3]", "Err");
        test("[2 km] + [3]", "Err");
    }

    #[test]
    fn test_matrix_sub() {
        test("[2] - [3]", "[-1]");
        test("[2, 3] - [4, 5]", "[-2, -2]");
        test("[2, 3, 4] - [5, 6, 7]", "[-3, -3, -3]");
        test("[4; 5] - [2; 3]", "[2; 2]");

        test("[2 km] - [3]", "Err");
    }

    #[test]
    fn test_matrix_scalar_mult() {
        test("3 * [2]", "[6]");
        test("[2] * 6", "[12]");

        test("2 * [2, 3]", "[4, 6]");
        test("2 * [2, 3, 4]", "[4, 6, 8]");
        test("2 * [2; 3]", "[4; 6]");
        test("2 * [2, 3; 4, 5]", "[4, 6; 8, 10]");
        test("[2, 3; 4, 5] * 2", "[4, 6; 8, 10]");

        test("2km * [2]", "[4 km]");
    }

    #[test]
    fn div_by_zero() {
        test("1 / 0", "Err");
        test("1kg / 0", "Err");
        test("1m / 0s", "Err");
        test("1% / 0", "Err");
        test("10 / 0%", "Err");
    }

    #[test]
    fn test_matrix_scalar_div() {
        test("3 / [2]", "Err");
        test("[6] / 2", "[3]");

        test("[6, 10] / 2", "[3, 5]");
        test("[2, 3, 4] / 2", "[1, 1.5, 2]");
        test("[2; 3] / 2", "[1; 1.5]");
        test("[2, 3; 4, 5] / 2", "[1, 1.5; 2, 2.5]");

        test("[100g] / 2g", "[50]");
    }

    #[test]
    fn test_matrix_matrix_mult() {
        test("[3] * [2]", "[6]");
        test("[2;3] * [4, 5]", "[8, 10; 12, 15]");

        test(
            "[1,2,3,4; 5,6,7,8; 9,10,11,12; 13,14,15,16] * [30;40;50;60]",
            "[500; 1220; 1940; 2660]",
        );

        test(
            "[2,3,4,5] * [2,3,4,5; 6,7,8,9; 10,11,12,13; 14,15,16,17]",
            "[132, 146, 160, 174]",
        );
        test("[3m] * [2cm]", "[0.06 m^2]");

        test("[2,3] * [4]", "Err");
    }

    #[test]
    fn matrix_unit() {
        test("[2cm,3mm; 4m,5km] in m", "[0.02 m, 0.003 m; 4 m, 5000 m]");
    }

    #[test]
    fn kcal_unit_tokens() {
        test_tokens(
            "1 cal in J",
            &[
                num(1),
                str(" "),
                apply_to_prev_token_unit("cal"),
                str(" "),
                op(OperatorTokenType::UnitConverter),
                str(" "),
                unit("J"),
            ],
        );
    }

    #[test]
    fn kcal_unit() {
        test("1 cal in J", "4.1868 J");
        test("3kcal in J", "12560.4 J");
    }

    #[test]
    fn test_eval_failure_changes_token_type() {
        test_tokens(
            "1 - not_variable",
            &[num(1), str(" "), str("-"), str(" "), str("not_variable")],
        );
    }

    #[test]
    fn test_matrix_wont_take_operands_from_outside_its_scope() {
        test("1 + [2, asda]", "1");
    }

    #[test]
    fn test_bitwise_ops() {
        test("0xFF AND 0b111", "7");

        test_tokens(
            "0xFF AND(0b11 OR 0b1111)",
            &[
                num(0xff),
                str(" "),
                op(OperatorTokenType::BinAnd),
                op(OperatorTokenType::ParenOpen),
                num(0b11),
                str(" "),
                op(OperatorTokenType::BinOr),
                str(" "),
                num(0b1111),
                op(OperatorTokenType::ParenClose),
            ],
        );

        test("0xFF AND(0b11 OR 0b1111)", "15");
    }

    #[test]
    fn test_unfinished_operators() {
        test_tokens(
            "0xFF AND 0b11 AND",
            &[
                num(0xff),
                str(" "),
                op(OperatorTokenType::BinAnd),
                str(" "),
                num(0b11),
                str(" "),
                str("AND"),
            ],
        );
    }

    #[test]
    fn test_binary() {
        ///// binary
        // Kibi BIT!
        test("1 Kib in bits", "1024 bits");
        test("1 Kib in bytes", "128 bytes");
        test("1 Kib/s in b/s", "1024 b / s");

        test("1kb in bytes", "125 bytes");
    }

    #[test]
    fn test_variables() {
        let mut vars = create_vars();
        vars[0] = Some(Variable {
            name: Box::from(&['v', 'a', 'r'][..]),
            value: Ok(CalcResult::new(
                CalcResultType::Number(Decimal::from_str("12").unwrap()),
                0,
            )),
        });
        test_vars(&vars, "var * 2", "24", 0);
        test_vars(&vars, "var - var", "0", 0);
    }

    #[test]
    fn test_unit_cancelling() {
        test("1 km / 50m", "20");

        test_tokens(
            "1 km/m",
            &[num(1), str(" "), apply_to_prev_token_unit("km / m")],
        );
        test("1 km/m", "1000");
        test("1 m/km", "0.001");
        test_with_dec_count(100, "140k h/ month", "191.6495550992470910335272");

        test("1 m*km", "1000 m^2");
    }

    #[test]
    fn test_financial_without_dollar_sign() {
        test("2 year / 1 month", "24");
    }

    #[test]
    fn test_unit_money() {
        test_tokens(
            "10 $/month",
            &[num(10), str(" "), apply_to_prev_token_unit("$ / month")],
        );
        test("1 $/month", "1 $ / month");
        test("140k $ / month * 3 years", "5040000 $");
    }

    #[test]
    fn test_func_nth() {
        test("nth([5, 6, 7], 0)", "5");
        test("nth([5, 6, 7], 1)", "6");
        test("nth([5, 6, 7], 2)", "7");
    }

    #[test]
    fn test_missing_arg_nth_panic() {
        test_tokens(
            "nth(,[1])",
            &[
                op_err(OperatorTokenType::Fn {
                    arg_count: 0,
                    typ: FnType::Nth,
                }),
                op(OperatorTokenType::ParenOpen),
                op(OperatorTokenType::Comma),
                op(OperatorTokenType::Matrix {
                    row_count: 1,
                    col_count: 1,
                }),
                num(1),
                op(OperatorTokenType::BracketClose),
                op(OperatorTokenType::ParenClose),
            ],
        )
    }

    #[test]
    fn test_out_of_index_nth() {
        test_tokens(
            "nth([1],5)",
            &[
                op(OperatorTokenType::Fn {
                    arg_count: 0,
                    typ: FnType::Nth,
                }),
                op(OperatorTokenType::ParenOpen),
                op(OperatorTokenType::Matrix {
                    row_count: 1,
                    col_count: 1,
                }),
                num(1),
                op(OperatorTokenType::BracketClose),
                op(OperatorTokenType::Comma),
                num_with_err(5),
                op(OperatorTokenType::ParenClose),
            ],
        )
    }

    #[test]
    fn test_func_sum() {
        test("sum([5, 6, 7])", "18");
    }

    #[test]
    fn test_bitwise_not() {
        test("NOT(0b11)", "18446744073709551612");
        test("13 AND NOT(4 - 1)", "12");
    }

    #[test]
    fn test_func_transpose() {
        test("transpose([5, 6, 7])", "[5; 6; 7]");
        test("transpose([1, 2; 3, 4])", "[1, 3; 2, 4]");
        test("transpose([1, 2; 3, 4; 5, 6])", "[1, 3, 5; 2, 4, 6]");
    }

    #[test]
    fn test_func_pi() {
        test_with_dec_count(1000, "pi()", "3.1415926535897932384626433833");
        test("pi(1)", "Err");
    }

    #[test]
    fn test_func_e() {
        test_with_dec_count(1000, "e()", "2.7182818284590452353602874714");
        test("e(1)", "Err");
    }

    #[test]
    fn test_func_ln() {
        test_with_dec_count(1000, "ln(2)", "0.693147180559945");
        test_with_dec_count(1000, "ln(100)", "4.60517018598809");
        test("ln()", "Err");
        test("ln(2, 3)", "Err");
    }

    #[test]
    fn test_func_lg() {
        test_with_dec_count(1000, "lg(2)", "1");
        test_with_dec_count(1000, "lg(100)", "6.64385618977472");
        test("lg()", "Err");
        test("lg(2, 3)", "Err");
    }

    #[test]
    fn test_func_log() {
        test_with_dec_count(1000, "log(3, 2)", "0.630929753571457");
        test_with_dec_count(1000, "log(2, 100)", "6.64385618977473");
        test("log()", "Err");
        test("log(1)", "Err");
        test("log(1, 2, 3)", "Err");
    }

    #[test]
    fn test_func_cos() {
        test_with_dec_count(1000, "cos(2 degree)", "0.999390827019096");
        test_with_dec_count(1000, "cos(1 degree)", "0.999847695156391");
        test_with_dec_count(1000, "cos(1 rad)", "0.54030230586814");
        test("cos()", "Err");
        test("cos(1)", "Err");
        test("cos(1 rad^2)", "Err");
        test("cos(1, 2)", "Err");
    }

    #[test]
    fn test_func_sin() {
        test_with_dec_count(1000, "sin(2 degree)", "0.03489949670250097");
        test_with_dec_count(1000, "sin(1 degree)", "0.01745240643728351");
        test_with_dec_count(1000, "sin(1 rad)", "0.841470984807897");
        test("sin()", "Err");
        test("sin(1)", "Err");
        test("sin(1 rad^2)", "Err");
        test("sin(1, 2)", "Err");
        test("sin(1 m)", "Err");
        test_tokens(
            "sin(1 m)",
            &[
                op(OperatorTokenType::Fn {
                    arg_count: 0,
                    typ: FnType::Sin,
                }),
                op(OperatorTokenType::ParenOpen),
                num_with_err(1),
                str(" "),
                apply_to_prev_token_unit_with_err("m"),
                op(OperatorTokenType::ParenClose),
            ],
        );
    }

    #[test]
    fn test_func_acos() {
        test_with_dec_count(1000, "acos(1)", "0 rad");
        test_with_dec_count(1000, "acos(0.5)", "1.047197551196598 rad");
        test_with_dec_count(1000, "acos(-0.5)", "2.094395102393196 rad");
        test("acos()", "Err");
        test("acos(1 rad)", "Err");
        test("acos(1 degree)", "Err");
        test("acos(2)", "Err");
        test("acos(-2)", "Err");
        test("acos(1 rad^2)", "Err");
        test("acos(1, 2)", "Err");
    }

    #[test]
    fn test_func_asin() {
        test_with_dec_count(1000, "asin(1)", "1.570796326794897 rad");
        test_with_dec_count(1000, "asin(0.5)", "0.523598775598299 rad");
        test_with_dec_count(1000, "asin(-0.5)", "-0.523598775598299 rad");
        test("asin()", "Err");
        test("asin(1 rad)", "Err");
        test("asin(1 degree)", "Err");
        test("asin(2)", "Err");
        test("asin(-2)", "Err");
        test("asin(1 rad^2)", "Err");
        test("asin(1, 2)", "Err");
    }

    #[test]
    fn test_func_tan() {
        test_with_dec_count(1000, "tan(2 degree)", "0.03492076949174773");
        test_with_dec_count(1000, "tan(1 degree)", "0.01745506492821759");
        test_with_dec_count(1000, "tan(1 rad)", "1.557407724654902");
        test("tan()", "Err");
        test("tan(1)", "Err");
        test("tan(1 rad^2)", "Err");
        test("tan(1, 2)", "Err");
    }

    #[test]
    fn test_func_atan() {
        test_with_dec_count(1000, "atan(1)", "0.785398163397448 rad");
        test_with_dec_count(1000, "atan(0.5)", "0.463647609000806 rad");
        test_with_dec_count(1000, "atan(-0.5)", "-0.463647609000806 rad");
        test("atan()", "Err");
        test("atan(1 rad)", "Err");
        test("atan(1 degree)", "Err");
        test("atan(2)", "Err");
        test("atan(-2)", "Err");
        test("atan(1 rad^2)", "Err");
        test("atan(1, 2)", "Err");
    }

    #[test]
    fn test_func_abs() {
        test_with_dec_count(1000, "abs(10)", "10");
        test_with_dec_count(1000, "abs(-10)", "10");
        test("abs()", "Err");
        test("abs(1, 2)", "Err");
    }

    #[test]
    fn test_fraction_reduction_rounding() {
        test_with_dec_count(1000, "0.0030899999999999999999999999", "0.003090");
    }

    #[test]
    fn test_fraction_reduction_rounding2() {
        test_with_dec_count(1000, "5 m^2/s in km^2/h", "0.0180 km^2 / h");
    }

    #[test]
    fn test_single_brackets() {
        test("[", " ");
        test("]", " ");
        test("(", " ");
        test(")", " ");
        test("=", " ");
    }

    #[test]
    fn test_error_for_pow_percent() {
        test_tokens(
            "30^5%",
            &[
                num_with_err(30),
                op_err(OperatorTokenType::Pow),
                num_with_err(5),
                op_err(OperatorTokenType::Perc),
            ],
        );
    }

    #[test]
    fn test_zero_negativ_pow() {
        test("0^-1", "Err");
    }

    #[test]
    fn test_simple_unit() {
        test("30 years", "30 year");
    }

    #[test]
    fn test_error_wrong_result_year_multiply() {
        test("30 years * 12(1/year)", "360");
        test("30 years * 12/year", "360");
    }

    #[test]
    fn test_unit_in_denominator() {
        test("12/year", "12 / year");
    }

    #[test]
    fn test_unit_in_denominator_tokens() {
        test_tokens(
            "12/year",
            &[num(12), op(OperatorTokenType::Div), unit("year")],
        );
    }

    #[test]
    fn test_unit_in_denominator_tokens2() {
        test_tokens(
            "1/12/year",
            &[
                num(1),
                op(OperatorTokenType::Div),
                num(12),
                op(OperatorTokenType::Div),
                unit("year"),
            ],
        );
    }

    #[test]
    fn test_unit_in_denominator_tokens_with_parens() {
        test_tokens(
            "(12/year)",
            &[
                op(OperatorTokenType::ParenOpen),
                num(12),
                op(OperatorTokenType::Div),
                unit("year"),
                op(OperatorTokenType::ParenClose),
            ],
        );
    }

    #[test]
    fn test_that_pow_has_higher_precedence_than_unit() {
        test_tokens(
            "10^24kg",
            &[
                num(10),
                op(OperatorTokenType::Pow),
                num(24),
                apply_to_prev_token_unit("kg"),
            ],
        );
    }

    #[test]
    fn test_huge_nums_in_scientific_form() {
        test("1e28", "10000000000000000000000000000");
        for i in 0..=28 {
            let input = format!("1e{}", i);
            let expected = format!("1{}", "0".repeat(i));
            test(&input, &expected);
        }
    }

    #[test]
    fn test_pi() {
        test("π", "3.1416");
    }

    #[test]
    fn test_multiple_equal_signs2() {
        test("=(Blq9h/Oq=7y^$o[/kR]*$*oReyMo-M++]", "7");
    }

    #[test]
    fn no_panic_huge_num_vs_num() {
        test(
            "79 228 162 514 264 337 593 543 950 335",
            "79228162514264337593543950335",
        );
        test(
            "79228162514264337593543950335 + 79228162514264337593543950335",
            "Err",
        );
        test(
            "-79228162514264337593543950335 - 79228162514264337593543950335",
            "Err",
        );
        test("10^28 * 10^28", "Err");
        test("10^28 / 10^-28", "Err");
    }

    #[test]
    fn no_panic_huge_num_vs_perc() {
        test("10^28 + 1000%", "Err");
        test("79228162514264337593543950335 + 1%", "Err");
        test("-79228162514264337593543950335 - (-1%)", "Err");
        test("10^28 - 1000%", "Err");
        test("10^28 * 1000%", "Err");
        test("10^28 / 1000%", "1000000000000000000000000000 %");
    }

    #[test]
    fn no_panic_huge_unit_vs_perc() {
        test("10^28m + 1000%", "Err");
        test("10^28m - 1000%", "Err");
        test("-79228162514264337593543950335m - (-1%)", "Err");
        test("10^28m * 1000%", "Err");
        test("10^28m / 1000%", "Err");
    }

    #[test]
    fn no_panic_huge_perc_vs_perc() {
        test("10^28% + 1000%", "Err");
        test("10^28% - 1000%", "Err");
        test("10^28% * 1000%", "Err");
        test("10^28% / 1000%", "Err");
        test("-79228162514264337593543950335% - 1%", "Err");
    }

    #[test]
    fn no_panic_huge_unit_vs_unit() {
        test(
            "79228162514264337593543950335s + 79228162514264337593543950335s",
            "Err",
        );
        test(
            "-79228162514264337593543950335s - 79228162514264337593543950335s",
            "Err",
        );
    }

    #[test]
    fn test_multiplying_bug_numbers_via_unit_no_panic() {
        test("909636Yl", "909636 Yl");
    }

    #[test]
    fn test_huge_unit_exponent() {
        test("6K^61595", "Err");
    }

    #[test]
    fn test_fuzzing_issue() {
        test("90-/9b^72^4", "Err");
    }

    #[test]
    fn calc_bug_period_calc() {
        test("(1000/month) + (2000/year)", "1166.6667 / month");
    }

    #[test]
    fn calc_bug_period_calc2() {
        test("((1000/month) + (2000/year)) * 12 month", "14000");
    }

    #[test]
    fn calc_bug_period_calc3() {
        test("50 000 / month * 1 year", "600000");
    }

    #[test]
    fn test_u64_hex_bitwise_and() {
        test("0xFF AND 0xFFFFFFFFFFFFFFFF", &0xFFu64.to_string());
    }

    #[test]
    fn test_u64_hex_bitwise_or() {
        test(
            "0xFF OR 0xFFFFFFFFFFFFFFFF",
            &0xFFFFFFFFFFFFFFFFu64.to_string(),
        );
    }

    #[test]
    fn test_u64_hex_bitwise_xor() {
        test(
            "0xFF XOR 0xFFFFFFFFFFFFFFFF",
            &0xFFFFFFFFFFFFFF00u64.to_string(),
        );
    }

    #[test]
    fn test_u64_hex_bitwise_shift_left() {
        test(
            "0x00FFFFFF_FFFFFFFF << 8",
            &0xFFFFFFFF_FFFFFF00u64.to_string(),
        );
    }

    #[test]
    fn test_u64_hex_bitwise_shift_right() {
        test(
            "0xFFFFFFFF_FFFFFFFF >> 8",
            &0x00FFFFFF_FFFFFFFFu64.to_string(),
        );
    }

    #[test]
    fn test_calc_num_perc_on_what() {
        test("41 is 17% on what", "35.0427");
    }

    #[test]
    fn test_calc_num_perc_on_what_tokens() {
        test_tokens(
            "41 is 17% on what",
            &[
                num(41),
                str(" "),
                op(OperatorTokenType::PercentageIs),
                str(" "),
                num(17),
                op(OperatorTokenType::Perc),
                str(" "),
                op(OperatorTokenType::Percentage_Find_Base_From_Result_Increase_X),
            ],
        );
    }

    #[test]
    fn test_calc_num_perc_on_what_2() {
        test("41 is (16%+1%) on what", "35.0427");
    }

    #[test]
    fn test_calc_num_perc_on_what_3() {
        test("41 is (16+1)% on what", "35.0427");
    }

    #[test]
    fn test_calc_percentage_what_plus() {
        test("what plus 17% is 41", "35.0427");
    }

    #[test]
    fn test_calc_percentage_what_plus_2() {
        test("what plus (16%+1%) is 41", "35.0427");
    }
    #[test]
    fn test_calc_percentage_what_plus_3() {
        test("what plus (16+1)% is 41", "35.0427");
    }

    #[test]
    fn test_calc_perc_on_what_is() {
        test("17% on what is 41", "35.0427");
    }

    #[test]
    fn test_calc_perc_on_what_is_tokens() {
        test_tokens(
            "17% on what is 41",
            &[
                num(17),
                op(OperatorTokenType::Perc),
                str(" "),
                op(OperatorTokenType::Percentage_Find_Base_From_Icrease_X_Result),
                str(" "),
                num(41),
            ],
        );
    }

    #[test]
    fn test_calc_perc_on_what_is_2() {
        test("(16%+1%) on what is 41", "35.0427");
    }

    #[test]
    fn test_calc_perc_on_what_is_3() {
        test("(16+1)% on what is 41", "35.0427");
    }

    #[test]
    fn test_calc_num_what_perc_on_num_tokens() {
        test("41 is what % on 35", "17.1429 %");
    }

    #[test]
    fn test_calc_num_perc_off_what() {
        test("41 is 17% off what", "49.3976");
    }

    #[test]
    fn test_calc_num_perc_off_what_tokens() {
        test_tokens(
            "41 is 17% off what",
            &[
                num(41),
                str(" "),
                op(OperatorTokenType::PercentageIs),
                str(" "),
                num(17),
                op(OperatorTokenType::Perc),
                str(" "),
                op(OperatorTokenType::Percentage_Find_Base_From_Result_Decrease_X),
            ],
        );
    }

    #[test]
    fn test_calc_num_perc_off_what_2() {
        test("41 is (16%+1%) off what", "49.3976");
    }

    #[test]
    fn test_calc_num_perc_off_what_3() {
        test("41 is (16+1)% off what", "49.3976");
    }

    #[test]
    fn test_calc_percentage_what_minus() {
        test("what minus 17% is 41", "49.3976");
    }

    #[test]
    fn test_calc_percentage_what_minus_2() {
        test("what minus (16%+1%) is 41", "49.3976");
    }
    #[test]
    fn test_calc_percentage_what_minus_3() {
        test("what minus (16+1)% is 41", "49.3976");
    }

    #[test]
    fn test_calc_perc_off_what_is() {
        test("17% off what is 41", "49.3976");
    }

    #[test]
    fn test_calc_perc_off_what_is_tokens() {
        test_tokens(
            "17% off what is 41",
            &[
                num(17),
                op(OperatorTokenType::Perc),
                str(" "),
                op(OperatorTokenType::Percentage_Find_Base_From_Decrease_X_Result),
                str(" "),
                num(41),
            ],
        );
    }

    #[test]
    fn test_calc_perc_off_what_is_2() {
        test("(16%+1%) off what is 41", "49.3976");
    }

    #[test]
    fn test_calc_perc_off_what_is_3() {
        test("(16+1)% off what is 41", "49.3976");
    }

    #[test]
    fn test_calc_num_what_perc_off_num_tokens() {
        test("35 is what % off 41", "14.6341 %");
    }

    #[test]
    fn test_percent_complex_1() {
        test("44 is (220 is what % on 200) on what", "40");
    }

    #[test]
    fn test_percent_complex_2() {
        test("44 is (180 is what % off 200) on what", "40");
    }

    #[test]
    fn test_percent_complex_3() {
        test("(44 is 10% on what) is 60% on what", "25");
    }

    #[test]
    fn test_percent_complex_4() {
        test("what plus (180 is what % off 200) is 44", "40");
    }

    #[test]
    fn test_percent_complex_5() {
        test("(180 is what % off 200) on what is 44", "40");
    }

    #[test]
    fn test_percent_complex_6() {
        test(
            "44 is what % on ((180 is what % off 200) on what is 44)",
            "10 %",
        );
    }

    #[test]
    fn test_percent_complex_7() {
        test(
            "44 is what % on ((180 is what % off (what plus 10% is 220)) on what is 44)",
            "10 %",
        );
    }

    #[test]
    fn test_calc_percentage_find_rate_from_result_base() {
        test("20 is what percent of 60", "33.3333 %");
    }

    #[test]
    fn test_calc_percentage_find_rate_from_result_base_tokens() {
        test_tokens(
            "20 is what percent of 60",
            &[
                num(20),
                str(" "),
                op(OperatorTokenType::Percentage_Find_Rate_From_Result_Base),
                str(" "),
                num(60),
            ],
        );
    }

    #[test]
    fn test_calc_percentage_find_base_from_result_rate() {
        test("5 is 25% of what", "20");
    }

    #[test]
    fn test_calc_percentage_find_base_from_result_rate_tokens() {
        test_tokens(
            "5 is 25% of what",
            &[
                num(5),
                str(" "),
                op(OperatorTokenType::PercentageIs),
                str(" "),
                num(25),
                op(OperatorTokenType::Perc),
                str(" "),
                op(OperatorTokenType::Percentage_Find_Base_From_Result_Rate),
            ],
        );
    }

    #[test]
    fn test_invalid_left_shift_to_string2() {
        test("1 << 200", "Err");
    }

    #[test]
    fn test_invalid_left_shift_to_string() {
        test_tokens(
            "1 << 200",
            &[
                num_with_err(1),
                str(" "),
                op_err(OperatorTokenType::ShiftLeft),
                str(" "),
                num_with_err(200),
            ],
        );
    }

    #[test]
    fn test_invalid_right_shift_to_string2() {
        test("1 >> 64", "Err");
    }

    #[test]
    fn test_invalid_right_shift_to_string() {
        test_tokens(
            "1 >> 64",
            &[
                num_with_err(1),
                str(" "),
                op_err(OperatorTokenType::ShiftRight),
                str(" "),
                num_with_err(64),
            ],
        );
    }

    #[test]
    fn test_multiplying_too_much_units() {
        test("1 km*h*s*b*J*A*ft * 2 L*mi", "Err");
    }

    #[test]
    fn test_dividing_too_much_units() {
        test("1 km*h*s*b*J*A*ft / 2 L*mi", "Err");
    }

    #[test]
    fn test_unit_conversion_26() {
        test_tokens(
            "(256byte * 120) in MiB",
            &[
                op(OperatorTokenType::ParenOpen),
                num(256),
                apply_to_prev_token_unit("bytes"),
                str(" "),
                op(OperatorTokenType::Mult),
                str(" "),
                num(120),
                op(OperatorTokenType::ParenClose),
                str(" "),
                op(OperatorTokenType::UnitConverter),
                str(" "),
                unit("MiB"),
            ],
        );
    }

    #[test]
    fn test_explicit_multipl_is_mandatory_before_units() {
        test_tokens(
            "2m^4kg/s^3",
            &[num(2), apply_to_prev_token_unit("m^4"), str("kg/s^3")],
        );
        // it is the accepted form
        test_tokens(
            "2m^4*kg/s^3",
            &[num(2), apply_to_prev_token_unit("(m^4 kg) / s^3")],
        );
    }

    #[test]
    fn not_in_must_be_str_if_we_are_sure_it_cant_be_unit() {
        test_tokens(
            "12 m in",
            &[
                num(12),
                str(" "),
                apply_to_prev_token_unit("m"),
                str(" "),
                str("in"),
            ],
        );
        test("12 m in", "12 m");
    }

    #[test]
    fn test_bug_no_paren_around_100() {
        test_tokens(
            "1+e()^(100)",
            &[
                num(1),
                op(OperatorTokenType::Add),
                op_err(OperatorTokenType::Fn {
                    arg_count: 0,
                    typ: FnType::E,
                }),
                op(OperatorTokenType::ParenOpen),
                op(OperatorTokenType::ParenClose),
                op_err(OperatorTokenType::Pow),
                op(OperatorTokenType::ParenOpen),
                num_with_err(100),
                op(OperatorTokenType::ParenClose),
            ],
        );
    }

    #[test]
    fn test_fuzz_bug_201220() {
        test(")5)t[Mr/(K)", "5 t");
    }

    #[test]
    fn test_fuzz_bug_201221_2_no_panic_if_arg_is_not_valid_token() {
        test("e(R())", "Err");
    }

    #[test]
    fn test_fuzz_bug_201221_3_no_panic_if_arg_is_not_valid_token() {
        test("sin(R())", "Err");
    }

    #[test]
    fn test_fuzz_bug_201221_4_no_panic_if_arg_is_not_valid_token() {
        test("ln(R())", "Err");
        test("ln(R(), R())", "Err");
        test("ln()", "Err");
    }

    #[test]
    fn test_fuzz_bug_201221_5_no_panic_if_arg_is_not_valid_token() {
        test("log(R(), R())", "Err");
    }

    #[test]
    fn test_fuzz_bug_201221_6_no_panic_if_arg_is_not_valid_token() {
        test("ceil(R())", "Err");
    }

    #[test]
    fn test_fuzz_bug_201220_2() {
        test("[]8F(*^5+[2)]/)=^]0/", "[2]");
        test_tokens(
            "[]8F(*^5+[2)]/)=^]0/",
            &[
                str("["),
                str("]"),
                str("8"),
                str("F"),
                str("("),
                str("*"),
                str("^"),
                str("5"),
                str("+"),
                op(OperatorTokenType::Matrix {
                    row_count: 1,
                    col_count: 1,
                }),
                num(2),
                str(")"),
                op(OperatorTokenType::BracketClose),
                str("/"),
                str(")"),
                str("="),
                str("^"),
                str("]"),
                str("0"),
                str("/"),
            ],
        );
    }

    #[test]
    fn test_illegal_unary_minus_is_not_added_to_the_output() {
        test_tokens(
            "[7*7]*9#8=-+",
            &[
                op(OperatorTokenType::Matrix {
                    row_count: 1,
                    col_count: 1,
                }),
                num(7),
                op(OperatorTokenType::Mult),
                num(7),
                op(OperatorTokenType::BracketClose),
                str("*"),
                str("9"),
                str("#8"),
                str("="),
                str("-"),
                str("+"),
            ],
        );
    }

    // "str".split('').map(function(it){return 'str("'+it+'")';}).join(',')
}
