ALTER TABLE messages DROP CONSTRAINT messages_concat_if_pdu;
ALTER TABLE messages DROP CONSTRAINT messages_pdu_or_text;
ALTER TABLE messages DROP COLUMN text;
ALTER TABLE messages DROP COLUMN group_target;
ALTER TABLE messages ALTER COLUMN pdu SET NOT NULL;
DROP TABLE wa_persistence;
DROP TABLE groups;
