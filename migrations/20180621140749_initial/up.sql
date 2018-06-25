CREATE TABLE recipients (
	id SERIAL PRIMARY KEY,
	phone_number VARCHAR UNIQUE NOT NULL,
	nick VARCHAR NOT NULL
);
CREATE TABLE messages (
	id SERIAL PRIMARY KEY,
	phone_number VARCHAR NOT NULL,
	pdu bytea NOT NULL,
	csms_data INT
);
