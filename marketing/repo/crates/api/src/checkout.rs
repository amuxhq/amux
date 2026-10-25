use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use crate::{AppState, Error};

#[derive(Deserialize)]
pub struct CheckoutRequest {
    pub invoice_id: i64,
    pub card_token: String,
}

#[derive(Serialize)]
pub struct CheckoutResponse {
    pub payment_id: String,
    pub status: String,
}

pub async fn checkout(
    State(state): State<AppState>,
    Json(request): Json<CheckoutRequest>,
) -> Result<Json<CheckoutResponse>, Error> {
    let invoice = state.invoices.get(request.invoice_id).await?;
    let payment = state
        .payments
        .charge(&request.card_token, invoice.total_cents, &invoice.currency)
        .await?;
    Ok(Json(CheckoutResponse {
        payment_id: payment.id,
        status: payment.status.to_string(),
    }))
}
