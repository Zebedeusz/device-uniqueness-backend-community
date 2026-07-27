-- Availability and registration union People Chain allocations with every
-- durable reservation for one base. Keep that lookup proportional to the
-- base's discriminator set rather than the full outbox history.
CREATE INDEX username_reservations_base_digits_idx ON username_reservations (base, digits);
