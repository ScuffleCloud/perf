-- A health check table so we can can check if the database is reachable & writable.
CREATE TABLE health_check (
    id SERIAL PRIMARY KEY,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO health_check (id) VALUES (1);
