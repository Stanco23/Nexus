//! Expression engine for evaluating synthetic instrument formulas.
//!
//! Parses formula strings into an AST and evaluates them with slot values.

use std::fmt;

/// Token produced by the tokenizer.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Number(f64),
    Identifier(String),
    Op(char),
    LParen,
    RParen,
    Comma,
}

/// Expression AST node.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    Slot(usize),
    BinaryOp { op: char, left: Box<Expr>, right: Box<Expr> },
    UnaryOp { op: char, arg: Box<Expr> },
    FunctionCall { name: String, args: Vec<Expr> },
}

/// A compiled expression ready for evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledExpression {
    expr: Expr,
}

impl Default for CompiledExpression {
    fn default() -> Self {
        Self {
            expr: Expr::Number(0.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseExpressionError {
    UnexpectedChar(char, usize),
    MismatchedParen(usize),
    UnknownIdentifier(String),
    EmptyFormula,
}

impl fmt::Display for ParseExpressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseExpressionError::UnexpectedChar(c, pos) => {
                write!(f, "unexpected character '{}' at position {}", c, pos)
            }
            ParseExpressionError::MismatchedParen(pos) => {
                write!(f, "mismatched parenthesis at position {}", pos)
            }
            ParseExpressionError::UnknownIdentifier(name) => {
                write!(f, "unknown identifier '{}'", name)
            }
            ParseExpressionError::EmptyFormula => {
                write!(f, "empty formula")
            }
        }
    }
}

impl std::error::Error for ParseExpressionError {}

#[derive(Debug, Clone, PartialEq)]
pub enum EvalError {
    DivisionByZero,
    MissingSlot(usize),
    NumericError(String),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::DivisionByZero => write!(f, "division by zero"),
            EvalError::MissingSlot(idx) => write!(f, "missing slot {}", idx),
            EvalError::NumericError(msg) => write!(f, "numeric error: {}", msg),
        }
    }
}

impl std::error::Error for EvalError {}

/// Tokenizer for formula strings.
struct Tokenizer {
    input: Vec<char>,
    pos: usize,
}

impl Tokenizer {
    fn new(formula: &str) -> Self {
        Self {
            input: formula.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        if self.pos < self.input.len() {
            let ch = self.input[self.pos];
            self.pos += 1;
            Some(ch)
        } else {
            None
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn read_number(&mut self, ch: char) -> Result<Token, ParseExpressionError> {
        let mut digits = String::new();
        digits.push(ch);

        while let Some(&ch) = self.input.get(self.pos) {
            if ch.is_ascii_digit() || ch == '.' {
                digits.push(ch);
                self.advance();
            } else {
                break;
            }
        }

        digits.parse::<f64>()
            .map(Token::Number)
            .map_err(|_| ParseExpressionError::UnexpectedChar(digits.chars().next().unwrap(), self.pos - digits.len()))
    }

    fn read_identifier(&mut self, ch: char) -> Token {
        let mut name = String::new();
        name.push(ch);

        while let Some(&ch) = self.input.get(self.pos) {
            if ch.is_alphanumeric() || ch == '_' {
                name.push(ch);
                self.advance();
            } else {
                break;
            }
        }

        Token::Identifier(name)
    }

    fn next_token(&mut self) -> Result<Option<Token>, ParseExpressionError> {
        self.skip_whitespace();

        let Some(ch) = self.advance() else {
            return Ok(None);
        };

        let token = match ch {
            '+' => Token::Op('+'),
            '-' => {
                // Check if this is a negative number (followed by digit) or unary minus
                if let Some(&next_ch) = self.input.get(self.pos) {
                    if next_ch.is_ascii_digit() {
                        return self.read_number(ch).map(Some);
                    }
                }
                Token::Op('-')
            }
            '*' => Token::Op('*'),
            '/' => Token::Op('/'),
            '%' => Token::Op('%'),
            '(' => Token::LParen,
            ')' => Token::RParen,
            ',' => Token::Comma,
            '0'..='9' => self.read_number(ch)?,
            'a'..='z' | 'A'..='Z' | '_' => self.read_identifier(ch),
            c => return Err(ParseExpressionError::UnexpectedChar(c, self.pos - 1)),
        };

        Ok(Some(token))
    }

    fn tokenize(&mut self) -> Result<Vec<Token>, ParseExpressionError> {
        let mut tokens = Vec::new();
        while let Some(token) = self.next_token()? {
            tokens.push(token);
        }
        Ok(tokens)
    }
}

/// Parser for token streams.
struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<Token> {
        if self.pos < self.tokens.len() {
            let tok = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn parse_formula(formula: &str) -> Result<CompiledExpression, ParseExpressionError> {
        if formula.trim().is_empty() {
            return Err(ParseExpressionError::EmptyFormula);
        }
        let mut tokenizer = Tokenizer::new(formula);
        let tokens = tokenizer.tokenize()?;
        let mut parser = Parser::new(tokens);
        let expr = parser.parse_expr()?;
        Ok(CompiledExpression { expr })
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseExpressionError> {
        self.parse_term()
    }

    fn parse_term(&mut self) -> Result<Expr, ParseExpressionError> {
        let mut left = self.parse_factor()?;

        while let Some(Token::Op(op)) = self.peek() {
            let op_char = *op;
            if op_char == '+' || op_char == '-' {
                self.advance();
                let right = self.parse_factor()?;
                left = Expr::BinaryOp {
                    op: op_char,
                    left: Box::new(left),
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }

        Ok(left)
    }

    fn parse_factor(&mut self) -> Result<Expr, ParseExpressionError> {
        let mut left = self.parse_unary()?;

        while let Some(Token::Op(op)) = self.peek() {
            let op_char = *op;
            if op_char == '*' || op_char == '/' || op_char == '%' {
                self.advance();
                let right = self.parse_unary()?;
                left = Expr::BinaryOp {
                    op: op_char,
                    left: Box::new(left),
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }

        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseExpressionError> {
        if let Some(Token::Op(op)) = self.peek() {
            let op_char = *op;
            if op_char == '+' || op_char == '-' {
                self.advance();
                let arg = self.parse_unary()?;
                return Ok(Expr::UnaryOp { op: op_char, arg: Box::new(arg) });
            }
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseExpressionError> {
        let token = self.advance().ok_or(ParseExpressionError::UnexpectedChar('\0', self.pos))?;

        match token {
            Token::Number(n) => Ok(Expr::Number(n)),
            Token::LParen => {
                let expr = self.parse_expr()?;
                match self.advance() {
                    Some(Token::RParen) => Ok(expr),
                    Some(other) => Err(ParseExpressionError::UnexpectedChar(
                        match other {
                            Token::Op(c) => c,
                            Token::Number(_) => '0',
                            Token::Identifier(_) | Token::LParen | Token::RParen | Token::Comma => '(',
                        },
                        self.pos - 1,
                    )),
                    None => Err(ParseExpressionError::MismatchedParen(self.pos)),
                }
            }
            Token::Identifier(name) => {
                // Check for slot parsing (e.g., slot0, slot1, etc.)
                if let Some(rest) = name.strip_prefix("slot") {
                    if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                        if let Ok(idx) = rest.parse::<usize>() {
                            return Ok(Expr::Slot(idx));
                        }
                    }
                    // It starts with "slot" but isn't a valid slot reference
                    if rest.is_empty() {
                        return Err(ParseExpressionError::UnknownIdentifier(name));
                    }
                    // e.g., "slotX" where X isn't a digit
                    return Err(ParseExpressionError::UnknownIdentifier(name));
                }

                // Check for function call
                if let Some(Token::LParen) = self.peek() {
                    self.advance(); // consume '('
                    let mut args = Vec::new();

                    if let Some(Token::RParen) = self.peek() {
                        self.advance(); // consume ')'
                        return Ok(Expr::FunctionCall { name, args });
                    }

                    args.push(self.parse_expr()?);

                    while let Some(Token::Comma) = self.peek() {
                        self.advance();
                        args.push(self.parse_expr()?);
                    }

                    match self.advance() {
                        Some(Token::RParen) => Ok(Expr::FunctionCall { name, args }),
                        Some(other) => Err(ParseExpressionError::UnexpectedChar(
                            match other {
                                Token::Op(c) => c,
                                Token::Number(_) => '0',
                                Token::Identifier(_) | Token::LParen | Token::RParen | Token::Comma => '(',
                            },
                            self.pos - 1,
                        )),
                        None => Err(ParseExpressionError::MismatchedParen(self.pos)),
                    }
                } else {
                    Err(ParseExpressionError::UnknownIdentifier(name))
                }
            }
            Token::Op(c) => Err(ParseExpressionError::UnexpectedChar(c, self.pos - 1)),
            Token::RParen => Err(ParseExpressionError::MismatchedParen(self.pos - 1)),
            Token::Comma => Err(ParseExpressionError::UnexpectedChar(',', self.pos - 1)),
        }
    }
}

impl CompiledExpression {
    /// Parse a formula string into a compiled expression.
    pub fn parse_formula(formula: &str) -> Result<Self, ParseExpressionError> {
        Parser::parse_formula(formula)
    }

    /// Evaluate the expression with the given slot values.
    pub fn eval(&self, inputs: &[f64]) -> Result<f64, EvalError> {
        self.eval_expr(&self.expr, inputs)
    }

    fn eval_expr(&self, expr: &Expr, inputs: &[f64]) -> Result<f64, EvalError> {
        match expr {
            Expr::Number(n) => Ok(*n),
            Expr::Slot(idx) => {
                inputs
                    .get(*idx)
                    .copied()
                    .ok_or(EvalError::MissingSlot(*idx))
            }
            Expr::UnaryOp { op, arg } => {
                let val = self.eval_expr(arg, inputs)?;
                match op {
                    '+' => Ok(val),
                    '-' => Ok(-val),
                    _ => Err(EvalError::NumericError(format!("unknown unary operator '{}'", op))),
                }
            }
            Expr::BinaryOp { op, left, right } => {
                let l = self.eval_expr(left, inputs)?;
                let r = self.eval_expr(right, inputs)?;
                match op {
                    '+' => Ok(l + r),
                    '-' => Ok(l - r),
                    '*' => Ok(l * r),
                    '/' => {
                        if r == 0.0 {
                            Err(EvalError::DivisionByZero)
                        } else {
                            Ok(l / r)
                        }
                    }
                    '%' => {
                        if r == 0.0 {
                            Err(EvalError::DivisionByZero)
                        } else {
                            Ok(l % r)
                        }
                    }
                    _ => Err(EvalError::NumericError(format!("unknown binary operator '{}'", op))),
                }
            }
            Expr::FunctionCall { name, args } => {
                let arg_values: Result<Vec<f64>, _> = args.iter().map(|a| self.eval_expr(a, inputs)).collect();
                let vals = arg_values?;
                match name.as_str() {
                    "abs" => {
                        if args.len() != 1 {
                            return Err(EvalError::NumericError("abs expects 1 argument".to_string()));
                        }
                        Ok(vals[0].abs())
                    }
                    "min" => {
                        if vals.is_empty() {
                            return Err(EvalError::NumericError("min expects at least 1 argument".to_string()));
                        }
                        Ok(vals.iter().copied().fold(f64::INFINITY, f64::min))
                    }
                    "max" => {
                        if vals.is_empty() {
                            return Err(EvalError::NumericError("max expects at least 1 argument".to_string()));
                        }
                        Ok(vals.iter().copied().fold(f64::NEG_INFINITY, f64::max))
                    }
                    "round" => {
                        if args.len() != 1 {
                            return Err(EvalError::NumericError("round expects 1 argument".to_string()));
                        }
                        Ok(vals[0].round())
                    }
                    "floor" => {
                        if args.len() != 1 {
                            return Err(EvalError::NumericError("floor expects 1 argument".to_string()));
                        }
                        Ok(vals[0].floor())
                    }
                    "ceil" => {
                        if args.len() != 1 {
                            return Err(EvalError::NumericError("ceil expects 1 argument".to_string()));
                        }
                        Ok(vals[0].ceil())
                    }
                    _ => Err(EvalError::NumericError(format!("unknown function '{}'", name))),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_addition() {
        // Formula: (slot0 + slot1) / 2.0 with inputs [50000.0, 3000.0]
        // (50000 + 3000) / 2.0 = 26500
        let expr = CompiledExpression::parse_formula("(slot0 + slot1) / 2.0").unwrap();
        let result = expr.eval(&[50000.0, 3000.0]).unwrap();
        assert!((result - 26500.0).abs() < 1e-10, "Expected 26500.0 but got {}", result);
    }

    #[test]
    fn test_parse_multiplication() {
        let expr = CompiledExpression::parse_formula("(slot0 - slot1) * slot2").unwrap();
        let result = expr.eval(&[100.0, 50.0, 2.0]).unwrap();
        assert!((result - 100.0).abs() < 1e-10);
    }

    #[test]
    fn test_unexpected_char_error() {
        // "A + " has unknown identifier "A" followed by operator
        let result = CompiledExpression::parse_formula("A + ");
        assert!(result.is_err(), "Expected error for 'A + ', got {:?}", result);
    }

    #[test]
    fn test_division_by_zero() {
        let expr = CompiledExpression::parse_formula("slot0 / 0.0").unwrap();
        let result = expr.eval(&[100.0]);
        assert!(matches!(result, Err(EvalError::DivisionByZero)));
    }

    #[test]
    fn test_slot_parsing() {
        let expr = CompiledExpression::parse_formula("slot0 * slot1 + slot2").unwrap();
        let result = expr.eval(&[2.0, 3.0, 5.0]).unwrap();
        assert!((result - 11.0).abs() < 1e-10);
    }

    #[test]
    fn test_functions() {
        let expr = CompiledExpression::parse_formula("abs(slot0 - slot1)").unwrap();
        let result = expr.eval(&[100.0, 150.0]).unwrap();
        assert!((result - 50.0).abs() < 1e-10);

        let expr2 = CompiledExpression::parse_formula("min(slot0, slot1, slot2)").unwrap();
        let result2 = expr2.eval(&[3.0, 1.0, 2.0]).unwrap();
        assert!((result2 - 1.0).abs() < 1e-10);

        let expr3 = CompiledExpression::parse_formula("max(slot0, slot1)").unwrap();
        let result3 = expr3.eval(&[5.0, 10.0]).unwrap();
        assert!((result3 - 10.0).abs() < 1e-10);

        let expr4 = CompiledExpression::parse_formula("round(slot0)").unwrap();
        let result4 = expr4.eval(&[3.7]).unwrap();
        assert!((result4 - 4.0).abs() < 1e-10);

        let expr5 = CompiledExpression::parse_formula("floor(slot0)").unwrap();
        let result5 = expr5.eval(&[3.7]).unwrap();
        assert!((result5 - 3.0).abs() < 1e-10);

        let expr6 = CompiledExpression::parse_formula("ceil(slot0)").unwrap();
        let result6 = expr6.eval(&[3.2]).unwrap();
        assert!((result6 - 4.0).abs() < 1e-10);
    }

    #[test]
    fn test_empty_formula() {
        let result = CompiledExpression::parse_formula("");
        assert!(matches!(result, Err(ParseExpressionError::EmptyFormula)));

        let result2 = CompiledExpression::parse_formula("   ");
        assert!(matches!(result2, Err(ParseExpressionError::EmptyFormula)));
    }

    #[test]
    fn test_mismatched_paren() {
        let result = CompiledExpression::parse_formula("(slot0 + slot1");
        assert!(matches!(result, Err(ParseExpressionError::MismatchedParen(_))));
    }

    #[test]
    fn test_missing_slot() {
        let expr = CompiledExpression::parse_formula("slot0 + slot1").unwrap();
        let result = expr.eval(&[1.0]); // only slot0 provided
        assert!(matches!(result, Err(EvalError::MissingSlot(1))));
    }

    #[test]
    fn test_negative_numbers() {
        let expr = CompiledExpression::parse_formula("-slot0 + 5.0").unwrap();
        let result = expr.eval(&[10.0]).unwrap();
        assert!((result - (-5.0)).abs() < 1e-10);
    }

    #[test]
    fn test_complex_expression() {
        // (slot0 + slot1) * (slot2 - slot3) / slot4
        let expr = CompiledExpression::parse_formula("(slot0 + slot1) * (slot2 - slot3) / slot4").unwrap();
        let result = expr.eval(&[1.0, 2.0, 10.0, 5.0, 3.0]).unwrap();
        // (1 + 2) * (10 - 5) / 3 = 3 * 5 / 3 = 5.0
        assert!((result - 5.0).abs() < 1e-10);
    }

    #[test]
    fn test_modulo_operator() {
        let expr = CompiledExpression::parse_formula("slot0 % slot1").unwrap();
        let result = expr.eval(&[17.0, 5.0]).unwrap();
        assert!((result - 2.0).abs() < 1e-10);
    }
}