CREATE TABLE groups (
	id SERIAL PRIMARY KEY,
	jid VARCHAR UNIQUE NOT NULL,
	channel VARCHAR UNIQUE NOT NULL,
	participants INT[] NOT NULL,
	admins INT[] NOT NULL,
	topic VARCHAR NOT NULL
);
CREATE TABLE wa_persistence (
	rev INT PRIMARY KEY,
	data JSON NOT NULL
);
ALTER TABLE messages ALTER COLUMN pdu DROP NOT NULL;
ALTER TABLE messages ADD COLUMN group_target INT REFERENCES groups ON DELETE CASCADE;
ALTER TABLE messages ADD COLUMN text VARCHAR;
ALTER TABLE messages ADD CONSTRAINT messages_pdu_or_text CHECK ((pdu IS NULL) != (text IS NULL));
ALTER TABLE messages ADD CONSTRAINT messages_concat_if_pdu CHECK ((csms_data IS NULL) OR (pdu IS NULL));
