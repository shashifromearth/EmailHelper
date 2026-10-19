//! Print the questionnaire items found in an email body file.
fn main() {
    let path = std::env::args().nth(1).expect("usage: questions_check <file>");
    let body = std::fs::read_to_string(path).expect("readable file");
    for (i, q) in emailhelper::agents::questions::extract(&body).iter().enumerate() {
        println!("{:>2}. {q}", i + 1);
    }
}
