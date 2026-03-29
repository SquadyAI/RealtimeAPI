//! 数学计算器模块
//!
//! 为大语言模型提供精确的数学计算能力，支持：
//! - 基础四则运算 (+, -, *, /)
//! - 高级数学函数 (sin, cos, tan, log, exp, sqrt, pow)
//! - 常数 (pi, e)
//! - 复杂表达式解析
//! - 高精度计算

use crate::function_callback::{CallResult, FunctionCallbackError};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::f64::consts;
use tracing::{debug, error, info};

/// 数学计算结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MathResult {
    /// 原始表达式
    // pub equation: String,
    /// 计算结果
    pub result: f64,
}
// /// 结果的字符串表示（保持精度）
// pub result_string: String,
// /// 计算步骤（可选）
// pub steps: Option<Vec<String>>,
// /// 是否为近似值
// pub is_approximate: bool,
// /// 计算时间（毫秒）
// pub calculation_time_ms: u64,

/// 数学计算器
#[derive(Debug)]
pub struct MathCalculator {
    /// 最大递归深度（防止无限递归）
    max_recursion_depth: usize,
    /// 精度位数
    precision: usize,
}

impl Default for MathCalculator {
    fn default() -> Self {
        Self { max_recursion_depth: 100, precision: 15 }
    }
}

impl MathCalculator {
    /// 创建新的数学计算器
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置精度
    pub fn with_precision(mut self, precision: usize) -> Self {
        self.precision = precision;
        self
    }

    /// 计算数学表达式
    pub async fn calculate(&self, equation: &str) -> Result<MathResult, FunctionCallbackError> {
        let start_time = std::time::Instant::now();

        info!("🧮 开始计算表达式: {}", equation);

        // 预处理表达式
        let cleaned_expr = self.preprocess_expression(equation)?;
        debug!("🧮 预处理后的表达式: {}", cleaned_expr);

        // 解析和计算表达式
        let result = match self.evaluate_expression(&cleaned_expr, 0) {
            Ok(value) => value,
            Err(e) => {
                error!("🧮 计算失败: {} - 表达式: {}", e, equation);
                return Err(FunctionCallbackError::Other(format!("计算错误: {}", e)));
            },
        };

        let calculation_time = start_time.elapsed().as_millis() as u64;

        // 检查结果有效性
        if result.is_nan() {
            return Err(FunctionCallbackError::Other("计算结果为 NaN (非数字)".to_string()));
        }

        if result.is_infinite() {
            return Err(FunctionCallbackError::Other("计算结果为无穷大".to_string()));
        }

        // 格式化结果字符串
        // let result_string = self.format_result(result);
        // let is_approximate = self.is_approximate_result(result);

        let math_result = MathResult {
            // equation: equation.to_string(),
            result,
            // result_string: result_string.clone(),
            // is_approximate,
            // calculation_time_ms: calculation_time,
        };

        info!("🧮 计算完成: {} = {} (耗时: {}ms)", equation, result, calculation_time);

        Ok(math_result)
    }

    /// 预处理表达式
    fn preprocess_expression(&self, expr: &str) -> Result<String, FunctionCallbackError> {
        let mut result = expr.trim().to_lowercase();

        // 移除空格
        result = result.replace(' ', "");

        // 替换常见的数学符号和常数
        result = result.replace("π", "pi");
        result = result.replace("×", "*");
        result = result.replace("÷", "/");
        result = result.replace("**", "^"); // Python风格幂运算转数学风格
        result = result.replace("²", "^2");
        result = result.replace("³", "^3");

        // 处理隐式乘法 (例: 2pi -> 2*pi, 3(4+5) -> 3*(4+5))
        result = self.handle_implicit_multiplication(&result)?;

        // 验证表达式安全性
        self.validate_expression(&result)?;

        Ok(result)
    }

    /// 处理隐式乘法
    fn handle_implicit_multiplication(&self, expr: &str) -> Result<String, FunctionCallbackError> {
        let mut result = String::new();
        let chars: Vec<char> = expr.chars().collect();

        for i in 0..chars.len() {
            result.push(chars[i]);

            if i < chars.len() - 1 {
                let current = chars[i];
                let next = chars[i + 1];

                // 数字后跟字母或左括号 (例: 2pi, 3(4+5))
                // 右括号后跟数字或字母或左括号 (例: (2+3)4, (2+3)pi, (2+3)(4+5))
                if (current.is_ascii_digit() && (next.is_alphabetic() || next == '(')) || (current == ')' && (next.is_ascii_digit() || next.is_alphabetic() || next == '(')) {
                    result.push('*');
                }
            }
        }

        Ok(result)
    }

    /// 验证表达式安全性
    fn validate_expression(&self, expr: &str) -> Result<(), FunctionCallbackError> {
        // 检查危险字符
        let dangerous_chars = ['$', '@', '#', '&', '|', ';', '`'];
        for ch in dangerous_chars {
            if expr.contains(ch) {
                return Err(FunctionCallbackError::InvalidParameters(format!(
                    "表达式包含不安全字符: {}",
                    ch
                )));
            }
        }

        // 检查括号匹配
        let mut paren_count = 0;
        for ch in expr.chars() {
            match ch {
                '(' => paren_count += 1,
                ')' => {
                    paren_count -= 1;
                    if paren_count < 0 {
                        return Err(FunctionCallbackError::InvalidParameters("括号不匹配".to_string()));
                    }
                },
                _ => {},
            }
        }

        if paren_count != 0 {
            return Err(FunctionCallbackError::InvalidParameters("括号不匹配".to_string()));
        }

        Ok(())
    }

    /// 评估表达式
    fn evaluate_expression(&self, expr: &str, depth: usize) -> Result<f64, String> {
        if depth > self.max_recursion_depth {
            return Err("递归深度超限".to_string());
        }

        // 移除外层括号
        let expr = self.remove_outer_parentheses(expr);

        // 处理负号
        let expr = self.handle_unary_minus(&expr)?;

        // 按运算符优先级解析
        self.parse_expression(&expr, depth)
    }

    /// 移除外层括号
    fn remove_outer_parentheses(&self, expr: &str) -> String {
        let expr = expr.trim();
        if expr.starts_with('(') && expr.ends_with(')') {
            let mut paren_count = 0;
            let chars: Vec<char> = expr.chars().collect();

            for ch in &chars[..chars.len() - 1] {
                if *ch == '(' {
                    paren_count += 1;
                } else if *ch == ')' {
                    paren_count -= 1;
                    if paren_count == 0 {
                        // 如果不是最后一个字符就有配对，说明不是外层括号
                        return expr.to_string();
                    }
                }
            }

            // 如果到这里，说明是外层括号
            return expr[1..expr.len() - 1].to_string();
        }
        expr.to_string()
    }

    /// 处理一元负号
    fn handle_unary_minus(&self, expr: &str) -> Result<String, String> {
        if expr.starts_with('-') {
            return Ok(format!("0{}", expr));
        }
        Ok(expr.to_string())
    }

    /// 解析表达式（按运算符优先级）
    fn parse_expression(&self, expr: &str, depth: usize) -> Result<f64, String> {
        // 优先级从低到高：+-, //, */, ^, 函数调用

        // 1. 处理加减法
        if let Some((left, op, right)) = self.find_operator(expr, &['+', '-'])? {
            let left_val = self.evaluate_expression(&left, depth + 1)?;
            let right_val = self.evaluate_expression(&right, depth + 1)?;

            return match op {
                '+' => Ok(left_val + right_val),
                '-' => Ok(left_val - right_val),
                _ => unreachable!(),
            };
        }

        // 2. 处理整数除法
        if let Some((left, right)) = self.find_integer_division_operator(expr)? {
            let left_val = self.evaluate_expression(&left, depth + 1)?;
            let right_val = self.evaluate_expression(&right, depth + 1)?;

            if right_val == 0.0 {
                return Err("整数除法的除数不能为零".to_string());
            }

            let quotient = (left_val / right_val).trunc();
            return Ok(if quotient == -0.0 { 0.0 } else { quotient });
        }

        // 3. 处理乘除法和取余
        if let Some((left, op, right)) = self.find_operator(expr, &['*', '/', '%'])? {
            let left_val = self.evaluate_expression(&left, depth + 1)?;
            let right_val = self.evaluate_expression(&right, depth + 1)?;

            return match op {
                '*' => Ok(left_val * right_val),
                '/' => {
                    if right_val == 0.0 {
                        Err("除零错误".to_string())
                    } else {
                        Ok(left_val / right_val)
                    }
                },
                '%' => {
                    if right_val == 0.0 {
                        Err("取余运算的除数不能为零".to_string())
                    } else {
                        Ok(left_val % right_val)
                    }
                },
                _ => unreachable!(),
            };
        }

        // 4. 处理幂运算
        if let Some((left, _, right)) = self.find_operator(expr, &['^'])? {
            let left_val = self.evaluate_expression(&left, depth + 1)?;
            let right_val = self.evaluate_expression(&right, depth + 1)?;

            return Ok(left_val.powf(right_val));
        }

        // 5. 处理函数调用
        if let Some(result) = self.try_parse_function(expr, depth)? {
            return Ok(result);
        }

        // 6. 处理常数和数字
        self.parse_number_or_constant(expr)
    }

    /// 查找运算符（从右到左，考虑括号）
    fn find_operator(&self, expr: &str, operators: &[char]) -> Result<Option<(String, char, String)>, String> {
        let chars: Vec<char> = expr.chars().collect();
        let mut paren_count = 0;

        // 从右到左扫描，找到最后一个不在括号内的运算符
        for i in (0..chars.len()).rev() {
            match chars[i] {
                ')' => paren_count += 1,
                '(' => paren_count -= 1,
                ch if operators.contains(&ch) && paren_count == 0 => {
                    // 检查是否是负号（一元运算符）
                    if ch == '-' && (i == 0 || "+-*/^(".contains(chars[i - 1])) {
                        continue;
                    }
                    // 跳过整数除法的第二个斜杠，交由专门逻辑处理
                    if ch == '/' && i > 0 && chars.get(i - 1) == Some(&'/') {
                        continue;
                    }

                    let left = chars[..i].iter().collect::<String>();
                    let right = chars[i + 1..].iter().collect::<String>();

                    if left.is_empty() || right.is_empty() {
                        return Err(format!("运算符 '{}' 附近语法错误", ch));
                    }

                    return Ok(Some((left, ch, right)));
                },
                _ => {},
            }
        }

        Ok(None)
    }

    /// 查找整数除法运算符（//）
    fn find_integer_division_operator(&self, expr: &str) -> Result<Option<(String, String)>, String> {
        let chars: Vec<char> = expr.chars().collect();

        if chars.len() < 2 {
            return Ok(None);
        }

        let mut paren_count = 0;

        for i in (1..chars.len()).rev() {
            match chars[i] {
                ')' => paren_count += 1,
                '(' => paren_count -= 1,
                '/' if chars[i - 1] == '/' && paren_count == 0 => {
                    let left = chars[..i - 1].iter().collect::<String>();
                    let right = chars[i + 1..].iter().collect::<String>();

                    if left.is_empty() || right.is_empty() {
                        return Err("整数除法运算符 '//' 附近语法错误".to_string());
                    }

                    return Ok(Some((left, right)));
                },
                _ => {},
            }
        }

        Ok(None)
    }

    /// 尝试解析函数调用
    fn try_parse_function(&self, expr: &str, depth: usize) -> Result<Option<f64>, String> {
        // 查找函数模式: function_name(arguments)
        if let Some(paren_pos) = expr.find('(') {
            let func_name = &expr[..paren_pos];

            if !expr.ends_with(')') {
                return Err("函数调用语法错误：缺少右括号".to_string());
            }

            let args_str = &expr[paren_pos + 1..expr.len() - 1];

            // 解析参数
            let args = self.parse_function_arguments(args_str, depth)?;

            // 调用函数
            return Ok(Some(self.call_function(func_name, &args)?));
        }

        Ok(None)
    }

    /// 解析函数参数
    fn parse_function_arguments(&self, args_str: &str, depth: usize) -> Result<Vec<f64>, String> {
        if args_str.trim().is_empty() {
            return Ok(vec![]);
        }

        let mut args = Vec::new();
        let mut current_arg = String::new();
        let mut paren_count = 0;

        for ch in args_str.chars() {
            match ch {
                '(' => {
                    paren_count += 1;
                    current_arg.push(ch);
                },
                ')' => {
                    paren_count -= 1;
                    current_arg.push(ch);
                },
                ',' if paren_count == 0 => {
                    if !current_arg.trim().is_empty() {
                        args.push(self.evaluate_expression(&current_arg, depth + 1)?);
                        current_arg.clear();
                    }
                },
                _ => current_arg.push(ch),
            }
        }

        if !current_arg.trim().is_empty() {
            args.push(self.evaluate_expression(&current_arg, depth + 1)?);
        }

        Ok(args)
    }

    /// 调用数学函数
    fn call_function(&self, name: &str, args: &[f64]) -> Result<f64, String> {
        match name {
            // 三角函数
            "sin" => {
                if args.len() != 1 {
                    return Err("sin 函数需要 1 个参数".to_string());
                }
                Ok(args[0].sin())
            },
            "cos" => {
                if args.len() != 1 {
                    return Err("cos 函数需要 1 个参数".to_string());
                }
                Ok(args[0].cos())
            },
            "tan" => {
                if args.len() != 1 {
                    return Err("tan 函数需要 1 个参数".to_string());
                }
                Ok(args[0].tan())
            },
            "asin" | "arcsin" => {
                if args.len() != 1 {
                    return Err("asin 函数需要 1 个参数".to_string());
                }
                if args[0] < -1.0 || args[0] > 1.0 {
                    return Err("asin 参数必须在 [-1, 1] 范围内".to_string());
                }
                Ok(args[0].asin())
            },
            "acos" | "arccos" => {
                if args.len() != 1 {
                    return Err("acos 函数需要 1 个参数".to_string());
                }
                if args[0] < -1.0 || args[0] > 1.0 {
                    return Err("acos 参数必须在 [-1, 1] 范围内".to_string());
                }
                Ok(args[0].acos())
            },
            "atan" | "arctan" => {
                if args.len() != 1 {
                    return Err("atan 函数需要 1 个参数".to_string());
                }
                Ok(args[0].atan())
            },

            // 对数和指数函数
            "log" | "log10" => {
                if args.len() != 1 {
                    return Err("log 函数需要 1 个参数".to_string());
                }
                if args[0] <= 0.0 {
                    return Err("log 参数必须大于 0".to_string());
                }
                Ok(args[0].log10())
            },
            "ln" => {
                if args.len() != 1 {
                    return Err("ln 函数需要 1 个参数".to_string());
                }
                if args[0] <= 0.0 {
                    return Err("ln 参数必须大于 0".to_string());
                }
                Ok(args[0].ln())
            },
            "exp" => {
                if args.len() != 1 {
                    return Err("exp 函数需要 1 个参数".to_string());
                }
                Ok(args[0].exp())
            },

            // 幂和根函数
            "sqrt" => {
                if args.len() != 1 {
                    return Err("sqrt 函数需要 1 个参数".to_string());
                }
                if args[0] < 0.0 {
                    return Err("sqrt 参数不能为负数".to_string());
                }
                Ok(args[0].sqrt())
            },
            "pow" => {
                if args.len() != 2 {
                    return Err("pow 函数需要 2 个参数".to_string());
                }
                Ok(args[0].powf(args[1]))
            },

            // 其他函数
            "abs" => {
                if args.len() != 1 {
                    return Err("abs 函数需要 1 个参数".to_string());
                }
                Ok(args[0].abs())
            },
            "floor" => {
                if args.len() != 1 {
                    return Err("floor 函数需要 1 个参数".to_string());
                }
                Ok(args[0].floor())
            },
            "ceil" => {
                if args.len() != 1 {
                    return Err("ceil 函数需要 1 个参数".to_string());
                }
                Ok(args[0].ceil())
            },
            "round" => {
                if args.len() != 1 {
                    return Err("round 函数需要 1 个参数".to_string());
                }
                Ok(args[0].round())
            },
            "min" => {
                if args.len() < 2 {
                    return Err("min 函数需要至少 2 个参数".to_string());
                }
                Ok(args.iter().fold(f64::INFINITY, |a, &b| a.min(b)))
            },
            "max" => {
                if args.len() < 2 {
                    return Err("max 函数需要至少 2 个参数".to_string());
                }
                Ok(args.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b)))
            },

            _ => Err(format!("未知函数: {}", name)),
        }
    }

    /// 解析数字或常数
    fn parse_number_or_constant(&self, expr: &str) -> Result<f64, String> {
        let expr = expr.trim();

        // 常数
        match expr {
            "pi" | "π" => return Ok(consts::PI),
            "e" => return Ok(consts::E),
            "tau" | "τ" => return Ok(consts::TAU),
            _ => {},
        }

        // 数字
        expr.parse::<f64>().map_err(|_| format!("无法解析数字或常数: {}", expr))
    }

    /// 格式化结果
    #[allow(dead_code)]
    fn format_result(&self, result: f64) -> String {
        // 如果是整数，显示为整数
        if result.fract() == 0.0 && result.abs() < 1e15 {
            format!("{}", result as i64)
        } else {
            // 使用指定精度
            format!("{:.1$}", result, self.precision)
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string()
        }
    }

    /// 判断是否为近似结果
    #[allow(dead_code)]
    fn is_approximate_result(&self, result: f64) -> bool {
        // 如果结果包含三角函数、对数等可能产生无理数的运算，标记为近似值
        result.fract() != 0.0 && (result * 1e10).fract() != 0.0
    }
}

/// 数学计算功能实现
pub async fn calculate_function(parameters: &FxHashMap<String, serde_json::Value>) -> Result<CallResult, FunctionCallbackError> {
    let equation = parameters
        .get("equation")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FunctionCallbackError::InvalidParameters("缺少必需的 'equation' 参数".to_string()))?;

    // 检查表达式长度
    if equation.len() > 200 {
        return Err(FunctionCallbackError::InvalidParameters(
            "表达式过长（最大200字符）".to_string(),
        ));
    }

    let calculator = MathCalculator::new().with_precision(parameters.get("precision").and_then(|v| v.as_u64()).unwrap_or(15) as usize);

    let result = calculator.calculate(equation).await?;

    Ok(CallResult::Success(serde_json::Value::Number(
        serde_json::Number::from_f64(result.result).ok_or_else(|| FunctionCallbackError::Other("计算结果无法转换为JSON数字".to_string()))?,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_basic_arithmetic() {
        let calc = MathCalculator::new();

        // 基础四则运算
        assert_eq!(calc.calculate("2 + 3").await.unwrap().result, 5.0);
        assert_eq!(calc.calculate("10 - 4").await.unwrap().result, 6.0);
        assert_eq!(calc.calculate("6 * 7").await.unwrap().result, 42.0);
        assert_eq!(calc.calculate("15 / 3").await.unwrap().result, 5.0);
        assert_eq!(calc.calculate("200 % 60").await.unwrap().result, 20.0);
    }

    #[tokio::test]
    async fn test_complex_expressions() {
        let calc = MathCalculator::new();

        // 复杂表达式
        assert_eq!(calc.calculate("(2 + 3) * 4").await.unwrap().result, 20.0);
        assert_eq!(calc.calculate("2^3 + 1").await.unwrap().result, 9.0);
        assert!((calc.calculate("sin(pi/2)").await.unwrap().result - 1.0).abs() < 1e-10);
    }

    #[tokio::test]
    async fn test_functions() {
        let calc = MathCalculator::new();

        // 数学函数
        assert!((calc.calculate("sqrt(4)").await.unwrap().result - 2.0).abs() < 1e-10);
        assert!((calc.calculate("log(100)").await.unwrap().result - 2.0).abs() < 1e-10);
        assert!((calc.calculate("abs(-5)").await.unwrap().result - 5.0).abs() < 1e-10);
    }

    #[tokio::test]
    async fn test_pythagorean_expression() {
        let calc = MathCalculator::new();

        assert!((calc.calculate("sqrt(3^2 + 4^2)").await.unwrap().result - 5.0).abs() < 1e-10);
    }

    #[tokio::test]
    async fn test_constants() {
        let calc = MathCalculator::new();

        // 常数
        assert!((calc.calculate("pi").await.unwrap().result - std::f64::consts::PI).abs() < 1e-10);
        assert!((calc.calculate("e").await.unwrap().result - std::f64::consts::E).abs() < 1e-10);
    }

    #[tokio::test]
    async fn test_integer_division() {
        let calc = MathCalculator::new();

        assert_eq!(calc.calculate("400//60").await.unwrap().result, 6.0);
        assert_eq!(calc.calculate("5//2").await.unwrap().result, 2.0);
        assert_eq!(calc.calculate("-5//2").await.unwrap().result, -2.0);
    }

    #[tokio::test]
    async fn test_error_handling() {
        let calc = MathCalculator::new();

        // 错误处理
        assert!(calc.calculate("1 / 0").await.is_err());
        assert!(calc.calculate("sqrt(-1)").await.is_err());
        assert!(calc.calculate("log(-1)").await.is_err());
        assert!(calc.calculate("unknown_function(1)").await.is_err());
    }

    #[tokio::test]
    async fn test_calculate_function_with_modulo() {
        use rustc_hash::FxHashMap;

        let mut parameters = FxHashMap::default();
        parameters.insert("equation".to_string(), serde_json::Value::String("200 % 60".to_string()));

        let result = calculate_function(&parameters).await.unwrap();
        match result {
            CallResult::Success(value) => {
                assert_eq!(value.as_f64().unwrap(), 20.0);
            },
            _ => panic!("Expected success result"),
        }
    }
}
