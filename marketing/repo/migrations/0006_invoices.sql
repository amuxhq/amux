CREATE TABLE invoices (
    id BIGSERIAL PRIMARY KEY,
    number TEXT NOT NULL UNIQUE,
    client_id BIGINT NOT NULL REFERENCES clients (id),
    total_cents BIGINT NOT NULL,
    currency TEXT NOT NULL,
    line_items JSONB NOT NULL DEFAULT '[]'
);
