use babeldoc_parser_core::strict_page_identity::{observe, Uncertainty};
use lopdf::{dictionary, Document, Object};

fn pdf() -> Document {
    let mut pdf = Document::with_version("1.4");
    pdf.trailer.set("Root", Object::Reference((1, 0)));
    pdf.trailer.set("Size", 5);
    pdf.objects.insert((1, 0), Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference((2, 0)) }));
    pdf.objects.insert((2, 0), Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference((3, 0)), Object::Reference((4, 0))], "Count" => 2 }));
    for id in [3, 4] {
        pdf.objects.insert((id, 0), Object::Dictionary(dictionary! { "Type" => "Page", "Parent" => Object::Reference((2, 0)) }));
    }
    pdf
}

#[test]
fn observes_order_and_rejects_ambiguous_trees() {
    let original = pdf();
    assert_eq!(observe(&original, || false), Ok(vec![(3, 0), (4, 0)]));
    assert_eq!(observe(&original, || true), Err(Uncertainty::Cancelled));
    for kids in [vec![Object::Reference((3, 0)), Object::Reference((3, 0))], vec![Object::Reference((2, 0))], vec![Object::Reference((3, 0)), Object::Reference((9, 0))]] {
        let mut changed = pdf();
        changed.objects.get_mut(&(2, 0)).unwrap().as_dict_mut().unwrap().set("Kids", kids);
        assert!(observe(&changed, || false).is_err());
    }
    let mut changed = pdf();
    changed.objects.get_mut(&(2, 0)).unwrap().as_dict_mut().unwrap().set("Count", 1);
    assert!(observe(&changed, || false).is_err());
    let mut changed = pdf();
    changed.objects.get_mut(&(3, 0)).unwrap().as_dict_mut().unwrap().set("Parent", Object::Reference((4, 0)));
    assert!(observe(&changed, || false).is_err());
    assert_eq!(observe(&original, || false), Ok(vec![(3, 0), (4, 0)]));
}

#[test]
fn rejects_encrypted_and_previously_decrypted_documents() {
    use lopdf::encryption::{EncryptionState, EncryptionVersion, Permissions};
    let mut document = pdf();
    document.trailer.set("ID", vec![Object::string_literal("public-test-id"), Object::string_literal("public-test-id")]);
    let version = EncryptionVersion::V1 { document: &document, owner_password: "owner", user_password: "user", permissions: Permissions::all() };
    let state = EncryptionState::try_from(version).unwrap();
    document.encrypt(&state).unwrap();
    assert_eq!(observe(&document, || false), Err(Uncertainty::Invalid("encrypted document")));
    document.decrypt("user").unwrap();
    assert!(document.was_encrypted());
    assert_eq!(observe(&document, || false), Err(Uncertainty::Invalid("encrypted document")));
}

#[test]
fn rejects_catalog_and_page_tree_object_ids_outside_trailer_size() {
    let mut document = pdf();
    document.trailer.set("Size", 4);
    assert!(observe(&document, || false).is_err());

    let mut document = pdf();
    let catalog = document.objects.remove(&(1, 0)).unwrap();
    document.objects.insert((5, 0), catalog);
    document.trailer.set("Root", Object::Reference((5, 0)));
    assert!(observe(&document, || false).is_err());

    let mut document = pdf();
    let root = document.objects.remove(&(2, 0)).unwrap();
    document.objects.insert((5, 0), root);
    document.objects.get_mut(&(1, 0)).unwrap().as_dict_mut().unwrap().set("Pages", Object::Reference((5, 0)));
    for id in [3, 4] { document.objects.get_mut(&(id, 0)).unwrap().as_dict_mut().unwrap().set("Parent", Object::Reference((5, 0))); }
    assert!(observe(&document, || false).is_err());

    let mut document = pdf();
    let page = document.objects.remove(&(3, 0)).unwrap();
    document.objects.insert((0, 0), page);
    document.objects.get_mut(&(2, 0)).unwrap().as_dict_mut().unwrap().set("Kids", vec![Object::Reference((0, 0)), Object::Reference((4, 0))]);
    assert!(observe(&document, || false).is_err());
}
