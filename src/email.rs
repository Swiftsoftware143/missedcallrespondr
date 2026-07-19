use std::env;
use serde_json::json;

/// Send a password reset email via HTTP API (Mailgun, SendGrid, SMTP.com, etc.)
/// Uses environment variables for configuration so you can swap providers.
///
/// Required env vars:
///   EMAIL_API_URL - The HTTP endpoint (default: uses Mailgun format)
///   EMAIL_API_KEY - API key for the provider
///   EMAIL_FROM    - From address (default: swiftsoftware143@yahoo.com)
///
/// Mailgun example:
///   EMAIL_API_URL=https://api.mailgun.net/v3/mg.funnelswift.net/messages
///   EMAIL_API_KEY=key-xxxxxxxxx
///
/// SMTP.com example:
///   EMAIL_API_URL=https://api.smtp.com/v4/messages
///   EMAIL_API_KEY=your-smtpcom-key

/// Send a welcome email to a new user with their temporary password.
pub async fn send_welcome_email(to: &str, name: &str, password: &str) -> Result<(), String> {
    let api_url = env::var("EMAIL_API_URL")
        .map_err(|_| "EMAIL_API_URL not set".to_string())?;
    let api_key = env::var("EMAIL_API_KEY")
        .map_err(|_| "EMAIL_API_KEY not set".to_string())?;
    let from = env::var("EMAIL_FROM")
        .unwrap_or_else(|_| "swiftsoftware143@yahoo.com".to_string());

    let body = format!(
        "Welcome to MissedCall Respondr, {0}!\n\nYour account has been created successfully.\n\nHere are your login credentials:\n\nEmail: {1}\nPassword: {2}\n\nLogin at: https://app.missedcallrespondr.com/login\n\nYou can now:\n- Set up your missed call responses\n- Configure call forwarding rules\n- Monitor your call activity\n\nFor help, contact support@missedcallrespondr.com\n\nBest regards,\nThe MissedCall Respondr Team",
        name,
        to,
        password,
    );

    let payload = json!({
        "from": from,
        "to": to,
        "subject": "Welcome to MissedCall Respondr!",
        "text": body
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Failed to send welcome email: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Email API returned {}: {}", status, text));
    }

    Ok(())
}

/// Send a purchase confirmed email after successful payment.
pub async fn send_purchase_confirmed_email(to: &str, name: &str, plan_name: &str) -> Result<(), String> {
    let api_url = env::var("EMAIL_API_URL")
        .map_err(|_| "EMAIL_API_URL not set".to_string())?;
    let api_key = env::var("EMAIL_API_KEY")
        .map_err(|_| "EMAIL_API_KEY not set".to_string())?;
    let from = env::var("EMAIL_FROM")
        .unwrap_or_else(|_| "swiftsoftware143@yahoo.com".to_string());

    let body = format!(
        "Hi {0},\n\nThank you for your purchase! Your payment for the {1} plan has been received successfully.\n\nYou can access your dashboard at: https://app.missedcallrespondr.com/dashboard\n\nIf you have any questions, please contact support@missedcallrespondr.com\n\nBest regards,\nThe MissedCall Respondr Team",
        name,
        plan_name,
    );

    let payload = json!({
        "from": from,
        "to": to,
        "subject": "Payment Received - Thank You!",
        "text": body
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Failed to send purchase confirmed email: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Email API returned {}: {}", status, text));
    }

    Ok(())
}

pub async fn send_reset_email(to: &str, token: &str) -> Result<(), String> {
    let api_url = env::var("EMAIL_API_URL")
        .map_err(|_| "EMAIL_API_URL not set".to_string())?;
    let api_key = env::var("EMAIL_API_KEY")
        .map_err(|_| "EMAIL_API_KEY not set".to_string())?;
    let from = env::var("EMAIL_FROM")
        .unwrap_or_else(|_| "swiftsoftware143@yahoo.com".to_string());

    let body = format!(
        "Your password reset code is: {}\n\nThis code expires in 1 hour.\n\nIf you did not request this password reset, please ignore this email.\n\n- SwiftSoftware",
        token
    );

    let payload = json!({
        "from": from,
        "to": to,
        "subject": "Password Reset Request",
        "text": body
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Failed to send email request: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Email API returned {}: {}", status, text));
    }

    Ok(())
}
