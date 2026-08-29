use std::io::{BufRead, IsTerminal, Write};

pub fn is_tty() -> bool {
    std::io::stdin().is_terminal()
}

/// Interactive selection fires only on a real TTY
pub fn interactive() -> bool {
    is_tty() && !cfg!(test)
}

/// Show a numbered picker and return the chosen index, or `None` if the user
/// declined (empty answer / EOF).
pub fn pick(prompt: &str, options: &[String]) -> Option<usize> {
    println!("{prompt}");
    for (i, o) in options.iter().enumerate() {
        println!("[{}] {o}", i + 1);
    }
    println!("[n] none");
    print!("> ");
    let _ = std::io::stdout().flush();

    let mut line = String::new();
    let n = std::io::stdin().lock().read_line(&mut line).ok()?;
    if n == 0 {
        return None;
    }
    match line.trim().to_lowercase().as_str() {
        "n" | "" => None,
        s => s
            .parse::<usize>()
            .ok()
            .filter(|i| *i >= 1 && *i <= options.len())
            .map(|i| i - 1),
    }
}
