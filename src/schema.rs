table! {
    groups (id) {
        id -> Int4,
        jid -> Varchar,
        channel -> Varchar,
        participants -> Array<Int4>,
        admins -> Array<Int4>,
        topic -> Varchar,
    }
}

table! {
    messages (id) {
        id -> Int4,
        phone_number -> Varchar,
        pdu -> Nullable<Bytea>,
        csms_data -> Nullable<Int4>,
        group_target -> Nullable<Int4>,
        text -> Nullable<Varchar>,
        source -> Int4,
        ts -> Timestamp,
    }
}

table! {
    recipients (id) {
        id -> Int4,
        phone_number -> Varchar,
        nick -> Varchar,
        whatsapp -> Bool,
        avatar_url -> Nullable<Varchar>,
        notify -> Nullable<Varchar>,
    }
}

table! {
    wa_msgids (mid) {
        mid -> Varchar,
    }
}

table! {
    wa_persistence (rev) {
        rev -> Int4,
        data -> Json,
    }
}

joinable!(messages -> groups (group_target));

allow_tables_to_appear_in_same_query!(
    groups,
    messages,
    recipients,
    wa_msgids,
    wa_persistence,
);
