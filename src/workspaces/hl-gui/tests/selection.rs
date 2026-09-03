use hl_gui::{CollectionSelection, Event, EventId, NodeId, SelectedRow, SourceId, Version};

#[test]
fn collection_selection_keeps_position_separate_from_immutable_identity() {
    let event = Event::Select {
        node: NodeId::new(4),
        id: EventId::new("4:Select"),
        rows: vec![12],
        collection: Some(CollectionSelection {
            source: SourceId::new(9),
            version: Version::new(6),
            rows: vec![SelectedRow { index: 12, id: 4_200 }],
        }),
    };

    let Event::Select {
        rows,
        collection: Some(collection),
        ..
    } = event
    else {
        panic!("collection authority was lost");
    };
    assert_eq!(rows, vec![12], "legacy positions remain explicit");
    assert_eq!(collection.source, SourceId::new(9));
    assert_eq!(collection.version, Version::new(6));
    assert_eq!(collection.rows, vec![SelectedRow { index: 12, id: 4_200 }]);
}
