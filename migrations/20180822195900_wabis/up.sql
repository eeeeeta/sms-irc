ALTER TABLE recipients ADD COLUMN whatsapp BOOL NOT NULL DEFAULT false;
ALTER TABLE messages DROP CONSTRAINT IF EXISTS messages_concat_if_pdu;
