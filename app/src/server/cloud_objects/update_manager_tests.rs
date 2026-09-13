use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures_lite::future;
use settings::{RespectUserSyncSetting, SyncToCloud};
use warp_core::features::FeatureFlag;
use warp_graphql::object_permissions::AccessLevel;
use warp_graphql::scalars::time::ServerTimestamp;
use warpui::{App, ModelHandle, SingletonEntity};

use super::{GetCloudObjectResponse, InitialLoadResponse, UpdateManager};
use crate::ASSETS;
use crate::ai::cloud_environments::{
    AmbientAgentEnvironment, CloudAmbientAgentEnvironment, CloudAmbientAgentEnvironmentModel,
};
use crate::auth::user::TEST_USER_UID;
use crate::auth::{AuthStateProvider, UserUid};
use crate::cloud_object::model::actions::{
    ObjectAction, ObjectActionHistory, ObjectActionSubtype, ObjectActionType, ObjectActions,
};
use crate::cloud_object::model::generic_string_model::GenericStringObjectId;
use crate::cloud_object::model::persistence::{CloudModel, CloudModelEvent, UpdateSource};
use crate::cloud_object::{
    CloudObjectEventEntrypoint, CloudObjectGuest, CloudObjectLocation, CreateCloudObjectResult,
    CreatedCloudObject, GenericStringObjectFormat, JsonObjectType, ObjectDeleteResult,
    ObjectIdType, ObjectMetadataUpdateResult, ObjectPermissionsUpdateData, ObjectType, Owner,
    Revision, RevisionAndLastEditor, ServerCloudObject, ServerFolder, ServerGuestSubject,
    ServerNotebook, ServerObject, ServerObjectGuest, ServerPreference, ServerWorkflow,
    ServerWorkflowEnum, Space,
};
use crate::drive::CloudObjectTypeAndId;
use crate::drive::folders::{CloudFolder, CloudFolderModel, FolderId};
use crate::drive::sharing::{SharingAccessLevel, Subject, UserKind};
use crate::notebooks::{CloudNotebook, CloudNotebookModel, NotebookId};
use crate::persistence::ModelEvent;
use crate::server::cloud_objects::listener::ObjectUpdateMessage;
use crate::server::cloud_objects::test_utils::{
    UpdateManagerStruct, create_update_manager_struct, initialize_app, mock_server_api,
};
use crate::server::cloud_objects::update_manager::{
    ServerMetadata, ServerPermissions, get_duplicate_object_name,
};
use crate::server::ids::{
    ClientId, HashableId, ObjectUid, ServerId, ServerIdAndType, SyncId, ToServerId,
};
use crate::settings::{CloudPreferenceModel, Preference};
use crate::workflows::workflow::{Argument, ArgumentType, Workflow};
use crate::workflows::workflow_enum::{
    CloudWorkflowEnum, CloudWorkflowEnumModel, EnumVariants, WorkflowEnum,
};
use crate::workflows::{CloudWorkflow, CloudWorkflowModel, WorkflowId};
use crate::workspaces::user_profiles::{UserProfileWithUID, UserProfiles};
use crate::workspaces::user_workspaces::UserWorkspaces;

fn receive_object_update_from_rtc(
    app: &mut App,
    update_manager: &ModelHandle<UpdateManager>,
    item: ObjectUpdateMessage,
) {
    update_manager.update(app, move |update_manager, ctx| {
        update_manager.received_message_from_server(item, ctx);
    })
}

fn receive_initial_load_or_polling_update(
    app: &mut App,
    update_manager: &ModelHandle<UpdateManager>,
    force_refresh: bool,
    mocked_response: InitialLoadResponse,
) {
    update_manager.update(app, move |update_manager, ctx| {
        update_manager.on_changed_objects_fetched(mocked_response, force_refresh, ctx);
    })
}

fn mock_server_permissions(owner: Owner) -> ServerPermissions {
    ServerPermissions {
        space: owner,
        guests: Vec::new(),
        anyone_link_sharing: None,
        permissions_last_updated_ts: Utc::now().into(),
    }
}

fn create_workflow(
    client_id: ClientId,
    app: &mut App,
    update_manager: &ModelHandle<UpdateManager>,
) {
    create_workflow_internal(
        app,
        update_manager,
        client_id,
        "client_workflow".to_string(),
        "echo client".to_string(),
        Owner::mock_current_user(),
        None,
    )
}

fn create_workflow_internal(
    app: &mut App,
    update_manager: &ModelHandle<UpdateManager>,
    client_id: ClientId,
    workflow_name: String,
    workflow_command: String,
    owner: Owner,
    initial_folder_id: Option<SyncId>,
) {
    update_manager.update(app, |update_manager, ctx| {
        update_manager.create_workflow(
            Workflow::new(workflow_name, workflow_command),
            owner,
            initial_folder_id,
            client_id,
            CloudObjectEventEntrypoint::Unknown,
            true,
            ctx,
        );
    });
}

fn get_workflow(app: &App, sync_id: SyncId) -> CloudWorkflow {
    CloudModel::handle(app).read(app, |cloud_model, _ctx| {
        cloud_model
            .get_workflow(&sync_id)
            .expect("workflow should exist")
            .clone()
    })
}

fn mock_server_workflow(id: WorkflowId, owner: Owner, metadata: ServerMetadata) -> ServerWorkflow {
    ServerWorkflow::new(
        SyncId::ServerId(id.into()),
        CloudWorkflowModel::new(Workflow::new(format!("w{id}"), format!("c{id}"))),
        metadata,
        mock_server_permissions(owner),
    )
}

fn mock_server_workflow_with_enum(
    id: WorkflowId,
    enum_id: GenericStringObjectId,
    owner: Owner,
    metadata: ServerMetadata,
) -> (ServerWorkflow, ServerWorkflowEnum) {
    let workflow = ServerWorkflow::new(
        SyncId::ServerId(id.into()),
        CloudWorkflowModel::new(
            Workflow::new(format!("w{id}"), format!("c{id}")).with_arguments(vec![Argument {
                name: format!("e{enum_id}"),
                default_value: None,
                description: None,
                arg_type: ArgumentType::Enum {
                    enum_id: SyncId::ServerId(enum_id.into()),
                },
            }]),
        ),
        metadata.clone(),
        mock_server_permissions(owner),
    );

    let workflow_enum = ServerWorkflowEnum::new(
        SyncId::ServerId(enum_id.into()),
        CloudWorkflowEnumModel::new(WorkflowEnum {
            name: format!("e{id}"),
            is_shared: false,
            variants: EnumVariants::Static(vec!["v1".to_string(), "v2".to_string()]),
        }),
        metadata,
        mock_server_permissions(owner),
    );

    (workflow, workflow_enum)
}

fn mock_server_notebook(id: NotebookId, owner: Owner, metadata: ServerMetadata) -> ServerNotebook {
    ServerNotebook::new(
        SyncId::ServerId(id.into()),
        CloudNotebookModel {
            title: format!("n{id}"),
            data: format!("n{id}"),
            ai_document_id: None,
            conversation_id: None,
        },
        metadata,
        mock_server_permissions(owner),
    )
}

fn mock_server_folder(id: FolderId, owner: Owner, metadata: ServerMetadata) -> ServerFolder {
    ServerFolder::new(
        SyncId::ServerId(id.into()),
        CloudFolderModel {
            name: format!("f{id}"),
            is_open: false,
            is_warp_pack: false,
        },
        metadata,
        mock_server_permissions(owner),
    )
}

#[track_caller]
fn assert_pending_online_only_change_for_object(app: &mut App, uid: &ObjectUid, status: bool) {
    CloudModel::handle(app).update(app, |cloud_model, _| {
        if let Some(object) = cloud_model.get_mut_by_uid(uid) {
            assert_eq!(
                object.metadata().has_pending_online_only_change(),
                status,
                "Expected has_pending_online_only_change for {uid} to be {status}"
            );
        } else {
            panic!("object should have been in cloud model, but wasn't");
        }
    });
}

fn assert_pending_status_for_object(app: &mut App, uid: &ObjectUid, status: bool) {
    CloudModel::handle(app).update(app, |cloud_model, _| {
        if let Some(object) = cloud_model.get_mut_by_uid(uid) {
            assert_eq!(
                object.metadata().has_pending_content_changes(),
                status,
                "Expected has_pending_content_changes for {uid} to be {status}"
            );
        } else {
            panic!("object should have been in cloud model, but wasn't");
        }
    });
}

fn assert_trashed_status_for_object(app: &mut App, uid: &ObjectUid, is_trashed: bool) {
    CloudModel::handle(app).update(app, |cloud_model, _| {
        if let Some(object) = cloud_model.get_mut_by_uid(uid) {
            assert_eq!(
                object.metadata().trashed_ts.is_some(),
                is_trashed,
                "Expected trashed status for {uid} to be {is_trashed}"
            );
        } else {
            panic!("object should have been in cloud model, but wasn't");
        }
    });
}

fn assert_root_level_for_object(app: &mut App, uid: &ObjectUid, is_root_level: bool) {
    CloudModel::handle(app).update(app, |cloud_model, _| {
        if let Some(object) = cloud_model.get_mut_by_uid(uid) {
            assert_eq!(object.metadata().folder_id.is_none(), is_root_level);
        } else {
            panic!("object should have been in cloud model, but wasn't");
        }
    });
}

fn assert_folder_for_object(app: &App, uid: &ObjectUid, folder_id: Option<SyncId>) {
    CloudModel::handle(app).read(app, |cloud_model, _| {
        if let Some(object) = cloud_model.get_by_uid(uid) {
            assert_eq!(object.metadata().folder_id, folder_id);
        } else {
            panic!("object should have been in cloud model, but wasn't");
        }
    });
}

fn assert_space_for_object(app: &App, uid: &ObjectUid, space: Space) {
    CloudModel::handle(app).read(app, |cloud_model, ctx| {
        if let Some(object) = cloud_model.get_by_uid(uid) {
            assert_eq!(object.space(ctx), space);
        } else {
            panic!("object should have been in cloud model, but wasn't");
        }
    });
}

/// Asserts that the current editor email address of the object matches the expected value.
fn assert_current_editor_for_object(app: &mut App, uid: &ObjectUid, current_editor: Option<&str>) {
    app.read(|ctx| {
        if let Some(object) = CloudModel::as_ref(ctx).get_by_uid(uid) {
            assert_eq!(
                object.metadata().current_editor_uid.as_deref(),
                current_editor
            );
        } else {
            panic!("object {uid} should have been in cloud model, but wasn't");
        }
    });
}

/// Asserts that the `metadata_last_updated_ts` timestamp for an object has the expected value.
fn assert_metadata_ts_for_object(app: &mut App, uid: &ObjectUid, metadata_ts: ServerTimestamp) {
    app.read(|ctx| {
        if let Some(object) = CloudModel::as_ref(ctx).get_by_uid(uid) {
            assert_eq!(
                object.metadata().metadata_last_updated_ts,
                Some(metadata_ts)
            );
        } else {
            panic!("object {uid} should have been in cloud model, but wasn't");
        }
    });
}

/// Asserts that the `revision` timestamp for an object has the expected value.
fn assert_revision_for_object(app: &App, uid: &ObjectUid, revision: impl Into<Revision>) {
    let revision = revision.into();
    app.read(|ctx| {
        if let Some(object) = CloudModel::as_ref(ctx).get_by_uid(uid) {
            assert_eq!(object.metadata().revision, Some(revision));
        } else {
            panic!("object {uid} should have been in cloud model, but wasn't");
        }
    });
}

fn assert_notebook_data(app: &App, sync_id: SyncId, expected_data: &str) {
    CloudModel::handle(app).read(app, |cloud_model, _| {
        match cloud_model.get_notebook(&sync_id) {
            Some(notebook) => assert_eq!(&notebook.model().data, expected_data),
            None => panic!("notebook {sync_id:?} should have been in cloud model, but wasn't"),
        }
    })
}

fn db_events(update_manager_struct: &UpdateManagerStruct) -> Vec<ModelEvent> {
    let mut db_events = Vec::new();

    while let Ok(event) = update_manager_struct.receiver.try_recv() {
        db_events.push(event);
    }

    db_events
}

fn cloud_events(update_manager_struct: &UpdateManagerStruct) -> Vec<CloudModelEvent> {
    let mut events = Vec::new();
    while let Ok(event) = update_manager_struct.cloud_model_events.try_recv() {
        events.push(event);
    }
    events
}

/// Add a folder to the cloud model.
fn add_folder(id: FolderId, owner: Owner, cloud_model: &mut CloudModel) {
    cloud_model.add_object(
        id.into(),
        CloudFolder::new_from_server(mock_server_folder(
            id,
            owner,
            ServerMetadata {
                uid: ServerId::default(),
                revision: Revision::now(),
                metadata_last_updated_ts: Utc::now().into(),
                trashed_ts: None,
                folder_id: None,
                is_welcome_object: false,
                creator_uid: None,
                last_editor_uid: None,
                current_editor_uid: None,
            },
        )),
    )
}

// Runs a test case where we validate the sync state of an object after it's been created.

#[test]
fn test_metadata_after_trash_item_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let server_id: ServerId = 123.into();
        let workflow_id: WorkflowId = server_id.into();
        let sync_id = SyncId::ServerId(workflow_id.into());

        let workflow_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let workflow: ServerWorkflow = mock_server_workflow(
            workflow_id,
            Owner::mock_current_user(),
            workflow_metadata.clone(),
        );

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudWorkflow::new_from_server(workflow));
        });

        server_api
            .expect_trash_object()
            .times(1)
            .return_once(move |_| Ok(true));

        assert_trashed_status_for_object(&mut app, &sync_id.uid(), false);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));
        // trash the workflow
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.trash_object(type_and_id, ctx);
            });

        // Since we optimistically update trashed_ts, we should emit an event before the future
        // completes.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectTrashed {
                type_and_id,
                source: UpdateSource::Local
            }]
        );
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_trashed_status_for_object(&mut app, &sync_id.uid(), true);
        assert_pending_status_for_object(&mut app, &sync_id.uid(), false);

        // There shouldn't be a duplicate server event once trashing succeeds.
        assert!(cloud_events(&update_manager_struct).is_empty());
    });
}

#[test]
fn test_pending_metadata_update_with_rtc() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id: SyncId = SyncId::ServerId(notebook_id.into());

        let current_metadata_ts = Utc::now();
        let metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: Some("ian@warp.dev".to_string()),
        };
        let notebook: ServerNotebook =
            mock_server_notebook(notebook_id, Owner::mock_current_user(), metadata);

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // trash the notebook, but don't await the request.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Notebook);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.trash_object(type_and_id, ctx);
            });

        assert_pending_online_only_change_for_object(
            &mut app,
            &notebook_id.to_server_id().uid(),
            true,
        );

        let new_metadata_ts = current_metadata_ts + chrono::Duration::seconds(1);

        // While this trash request is "in-flight", mock getting an RTC update from the server that includes a new editor
        let mocked_metadata = ServerMetadata {
            uid: server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: new_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        let mocked_metadata_update_message = ObjectUpdateMessage::ObjectMetadataChanged {
            metadata: mocked_metadata.clone(),
        };
        receive_object_update_from_rtc(
            &mut app,
            &update_manager_struct.update_manager,
            mocked_metadata_update_message,
        );

        // Assert we don't have a pending change now that we've updated the metadata
        assert_pending_online_only_change_for_object(
            &mut app,
            &notebook_id.to_server_id().uid(),
            false,
        );

        // Assert that the metadata changes are correctly applied
        CloudModel::handle(&app).read(&app, |cloud_model, _ctx| {
            if let Some(object) = cloud_model.get_by_uid(&notebook_id.to_server_id().uid()) {
                assert_eq!(
                    object.metadata().current_editor_uid,
                    mocked_metadata.current_editor_uid
                );
                assert_eq!(
                    object
                        .metadata()
                        .metadata_last_updated_ts
                        .expect("metadata should exist"),
                    mocked_metadata.metadata_last_updated_ts
                );
            } else {
                panic!("object should have been in cloud model, but wasn't");
            }
        });

        let events = db_events(&update_manager_struct);

        assert_eq!(events.len(), 1);
        // we trigger a metadata update event from the rtc message
        assert!(matches!(
            &events[0],
            ModelEvent::UpdateObjectMetadata { id: _, metadata: _ }
        ));
        // No upsert should happen now since there's still the trash in flight
    })
}

#[test]
fn test_metadata_update_with_rtc_no_pending() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id: SyncId = SyncId::ServerId(notebook_id.into());

        let current_metadata_ts = Utc::now();
        let metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook =
            mock_server_notebook(notebook_id, Owner::mock_current_user(), metadata);

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        assert_pending_online_only_change_for_object(
            &mut app,
            &notebook_id.to_server_id().uid(),
            false,
        );

        let new_metadata_ts = current_metadata_ts + chrono::Duration::seconds(1);

        // While this trash request is "in-flight", mock getting an RTC update from the server that includes a new editor
        let mocked_metadata = ServerMetadata {
            uid: server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: new_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: Some("ian@warp.dev".to_string()),
        };

        let mocked_metadata_update_message = ObjectUpdateMessage::ObjectMetadataChanged {
            metadata: mocked_metadata.clone(),
        };
        receive_object_update_from_rtc(
            &mut app,
            &update_manager_struct.update_manager,
            mocked_metadata_update_message,
        );

        // Assert we still don't have a pending change now that we've updated the metadata
        assert_pending_online_only_change_for_object(
            &mut app,
            &notebook_id.to_server_id().uid(),
            false,
        );

        // Assert that the metadata changes are correctly applied
        CloudModel::handle(&app).read(&app, |cloud_model, _ctx| {
            if let Some(object) = cloud_model.get_by_uid(&notebook_id.to_server_id().uid()) {
                assert_eq!(
                    object.metadata().current_editor_uid,
                    mocked_metadata.current_editor_uid
                );
                assert_eq!(
                    object
                        .metadata()
                        .metadata_last_updated_ts
                        .expect("metadata should exist"),
                    mocked_metadata.metadata_last_updated_ts
                );
            } else {
                panic!("object should have been in cloud model, but wasn't");
            }
        });

        let events = db_events(&update_manager_struct);

        assert_eq!(events.len(), 1);
        // we trigger a metadata update event from the rtc message
        assert!(matches!(
            &events[0],
            ModelEvent::UpdateObjectMetadata { id: _, metadata: _ }
        ));
    })
}

#[test]
fn test_metadata_after_trash_item_failure() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let server_id: ServerId = 123.into();
        let workflow_id: WorkflowId = server_id.into();
        let sync_id = SyncId::ServerId(workflow_id.into());

        let workflow_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        let workflow: ServerWorkflow =
            mock_server_workflow(workflow_id, Owner::mock_current_user(), workflow_metadata);

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudWorkflow::new_from_server(workflow));
        });

        server_api
            .expect_trash_object()
            .returning(move |_| Err(anyhow::anyhow!("trash failed!")));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        assert_trashed_status_for_object(&mut app, &sync_id.uid(), false);

        // trash the workflow
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.trash_object(type_and_id, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // await long enough that all the trash object retries are exhausted
        warpui::r#async::Timer::after(Duration::from_secs(10)).await;

        assert_trashed_status_for_object(&mut app, &sync_id.uid(), false);
        assert_pending_status_for_object(&mut app, &sync_id.uid(), false);

        // There should be one event for the optimistic update and one for undoing it.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![
                CloudModelEvent::ObjectTrashed {
                    type_and_id,
                    source: UpdateSource::Local
                },
                CloudModelEvent::ObjectUntrashed {
                    type_and_id,
                    source: UpdateSource::Local
                }
            ]
        )
    });
}

#[test]
fn test_pending_metadata_update_with_polling() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let notebood_server_id: ServerId = 123.into();
        let notebook_id: NotebookId = notebood_server_id.into();
        let sync_id: SyncId = SyncId::ServerId(notebook_id.into());

        let current_metadata_ts = Utc::now();
        let metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook =
            mock_server_notebook(notebook_id, Owner::mock_current_user(), metadata);

        server_api
            .expect_trash_object()
            .times(1)
            .return_once(move |_| Ok(true));

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // trash the notebook, but don't await the request.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Notebook);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.trash_object(type_and_id, ctx);
            });
        assert_pending_online_only_change_for_object(
            &mut app,
            &notebook_id.to_server_id().uid(),
            true,
        );

        let new_metadata_ts = current_metadata_ts + chrono::Duration::seconds(1);
        // While this trash request is "in-flight", mock getting a polling update from the server that includes a new editor
        let mocked_metadata = ServerMetadata {
            uid: notebood_server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: new_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: Some("ian@warp.dev".to_string()),
        };

        // Add a generic object just for kicks and make sure it gets upserted
        let generic_object_server_id = 456.into();
        let mut updated_generic_string_objects: HashMap<
            GenericStringObjectFormat,
            Vec<Box<dyn ServerObject>>,
        > = HashMap::new();
        updated_generic_string_objects.insert(
            GenericStringObjectFormat::Json(JsonObjectType::Preference),
            vec![Box::new(ServerPreference::new(
                SyncId::ServerId(generic_object_server_id),
                CloudPreferenceModel::new(
                    Preference::new(
                        "test_storage_key".to_string(),
                        "{\"test_key\": \"test_value\"}",
                        SyncToCloud::Globally(RespectUserSyncSetting::Yes),
                    )
                    .expect("error creating preference"),
                ),
                mocked_metadata.clone(),
                mock_server_permissions(Owner::mock_current_user()),
            ))],
        );

        let mocked_response = InitialLoadResponse {
            updated_notebooks: vec![ServerNotebook::new(
                SyncId::ServerId(notebood_server_id),
                CloudNotebookModel {
                    title: "".into(),
                    data: "".into(),
                    ai_document_id: None,
                    conversation_id: None,
                },
                mocked_metadata.clone(),
                mock_server_permissions(Owner::mock_current_user()),
            )],
            deleted_notebooks: vec![],
            updated_workflows: vec![],
            deleted_workflows: vec![],
            updated_folders: vec![],
            deleted_folders: vec![],
            user_profiles: vec![],
            updated_generic_string_objects,
            deleted_generic_string_objects: Default::default(),
            action_histories: Default::default(),
            mcp_gallery: Default::default(),
        };
        receive_initial_load_or_polling_update(
            &mut app,
            &update_manager_struct.update_manager,
            false, /* force_refresh */
            mocked_response,
        );

        // Assert we don't have a pending change now that we've updated the metadata
        assert_pending_online_only_change_for_object(
            &mut app,
            &notebook_id.to_server_id().uid(),
            false,
        );

        // Assert that the metadata changes are correctly applied
        CloudModel::handle(&app).read(&app, |cloud_model, _ctx| {
            if let Some(object) = cloud_model.get_by_uid(&notebook_id.to_server_id().uid()) {
                assert_eq!(
                    object.metadata().current_editor_uid,
                    mocked_metadata.current_editor_uid
                );
                assert_eq!(
                    object
                        .metadata()
                        .metadata_last_updated_ts
                        .expect("metadata should exist"),
                    mocked_metadata.metadata_last_updated_ts
                );
            } else {
                panic!("object should have been in cloud model, but wasn't");
            }
        });

        let events = db_events(&update_manager_struct);

        // All the upserts from polling
        assert!(matches!(&events[0], ModelEvent::SyncObjectActions { .. }));
        assert!(matches!(&events[1], ModelEvent::UpsertNotebooks(_)));
        assert!(matches!(&events[2], ModelEvent::UpsertWorkflows(_)));
        assert!(matches!(&events[3], ModelEvent::UpsertFolders(_)));
        assert!(matches!(&events[4], ModelEvent::DeleteObjects { ids: _ }));
    });
}

#[test]
fn test_metadata_update_with_polling_no_pending() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id: SyncId = SyncId::ServerId(notebook_id.into());

        let current_metadata_ts = Utc::now();
        let metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook =
            mock_server_notebook(notebook_id, Owner::mock_current_user(), metadata);

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        assert_pending_online_only_change_for_object(
            &mut app,
            &notebook_id.to_server_id().uid(),
            false,
        );

        let new_metadata_ts = current_metadata_ts + chrono::Duration::seconds(1);
        // While this trash request is "in-flight", mock getting a polling update from the server that includes a new editor
        let mocked_metadata = ServerMetadata {
            uid: server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: new_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: Some("ian@warp.dev".to_string()),
        };
        let mocked_response = InitialLoadResponse {
            updated_notebooks: vec![ServerNotebook::new(
                SyncId::ServerId(server_id),
                CloudNotebookModel {
                    title: "".into(),
                    data: "".into(),
                    ai_document_id: None,
                    conversation_id: None,
                },
                mocked_metadata.clone(),
                mock_server_permissions(Owner::mock_current_user()),
            )],
            deleted_notebooks: vec![],
            updated_workflows: vec![],
            deleted_workflows: vec![],
            updated_folders: vec![],
            deleted_folders: vec![],
            user_profiles: vec![],
            updated_generic_string_objects: Default::default(),
            deleted_generic_string_objects: Default::default(),
            action_histories: Default::default(),
            mcp_gallery: Default::default(),
        };
        receive_initial_load_or_polling_update(
            &mut app,
            &update_manager_struct.update_manager,
            false, /* force_refresh */
            mocked_response,
        );

        // Assert we still don't have a pending change now that we've updated the metadata
        assert_pending_online_only_change_for_object(
            &mut app,
            &notebook_id.to_server_id().uid(),
            false,
        );

        // Assert that the metadata changes are correctly applied
        CloudModel::handle(&app).read(&app, |cloud_model, _ctx| {
            if let Some(object) = cloud_model.get_by_uid(&notebook_id.to_server_id().uid()) {
                assert_eq!(
                    object.metadata().current_editor_uid,
                    mocked_metadata.current_editor_uid
                );
                assert_eq!(
                    object
                        .metadata()
                        .metadata_last_updated_ts
                        .expect("metadata should exist"),
                    mocked_metadata.metadata_last_updated_ts
                );
            } else {
                panic!("object should have been in cloud model, but wasn't");
            }
        });

        let events = db_events(&update_manager_struct);

        // All the upserts from polling
        assert!(matches!(&events[0], ModelEvent::SyncObjectActions { .. }));
        assert!(matches!(&events[1], ModelEvent::UpsertNotebooks(_)));
        assert!(matches!(&events[2], ModelEvent::UpsertWorkflows(_)));
        assert!(matches!(&events[3], ModelEvent::UpsertFolders(_)));
        assert!(matches!(&events[4], ModelEvent::DeleteObjects { ids: _ }));
    });
}

#[test]
fn test_metadata_after_untrash_item_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let server_id: ServerId = 123.into();
        let workflow_id: WorkflowId = server_id.into();
        let sync_id = SyncId::ServerId(workflow_id.into());

        let workflow_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: Some(ServerTimestamp::from_unix_timestamp_micros(10).unwrap()),
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let workflow: ServerWorkflow = mock_server_workflow(
            workflow_id,
            Owner::mock_current_user(),
            workflow_metadata.clone(),
        );

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudWorkflow::new_from_server(workflow));
        });

        let mut untrashed_metadata = workflow_metadata;
        untrashed_metadata.trashed_ts = None;

        server_api
            .expect_untrash_object()
            .times(1)
            .return_once(move |_| {
                Ok(ObjectMetadataUpdateResult::Success {
                    metadata: Box::new(untrashed_metadata),
                })
            });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        assert_trashed_status_for_object(&mut app, &sync_id.uid(), true);

        // untrash the workflow
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.untrash_object(type_and_id, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_trashed_status_for_object(&mut app, &sync_id.uid(), false);
        assert_root_level_for_object(&mut app, &sync_id.uid(), true);
        assert_pending_status_for_object(&mut app, &sync_id.uid(), false);

        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectUntrashed {
                type_and_id,
                source: UpdateSource::Local
            }]
        );
    })
}

#[test]
fn test_metadata_after_untrash_item_and_move_to_root() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let server_id: ServerId = 123.into();
        let workflow_id: WorkflowId = server_id.into();
        let sync_id = SyncId::ServerId(workflow_id.into());
        let folder_id: FolderId = 456.into();

        let workflow_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: Some(ServerTimestamp::from_unix_timestamp_micros(10).unwrap()),
            folder_id: Some(folder_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let workflow: ServerWorkflow = mock_server_workflow(
            workflow_id,
            Owner::mock_current_user(),
            workflow_metadata.clone(),
        );

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudWorkflow::new_from_server(workflow));
        });

        let mut untrashed_metadata = workflow_metadata.clone();
        untrashed_metadata.trashed_ts = None;
        untrashed_metadata.folder_id = None;

        server_api
            .expect_untrash_object()
            .times(1)
            .return_once(move |_| {
                Ok(ObjectMetadataUpdateResult::Success {
                    metadata: Box::new(untrashed_metadata),
                })
            });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        assert_trashed_status_for_object(&mut app, &sync_id.uid(), true);
        assert_root_level_for_object(&mut app, &sync_id.uid(), false);

        // untrash the workflow
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.untrash_object(type_and_id, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_trashed_status_for_object(&mut app, &sync_id.uid(), false);
        assert_root_level_for_object(&mut app, &sync_id.uid(), true);
        assert_pending_status_for_object(&mut app, &sync_id.uid(), false);

        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectUntrashed {
                type_and_id,
                source: UpdateSource::Local
            }]
        );
    })
}

#[test]
fn test_metadata_after_untrash_item_failure() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let server_id: ServerId = 123.into();
        let workflow_id: WorkflowId = server_id.into();
        let sync_id = SyncId::ServerId(workflow_id.into());

        let workflow_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: Some(ServerTimestamp::from_unix_timestamp_micros(10).unwrap()),
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let workflow: ServerWorkflow =
            mock_server_workflow(workflow_id, Owner::mock_current_user(), workflow_metadata);

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudWorkflow::new_from_server(workflow));
        });

        // mock an unsuccessful untrashing attempt
        server_api
            .expect_untrash_object()
            .times(1)
            .return_once(move |_| Ok(ObjectMetadataUpdateResult::Failure));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // unsuccessfully attempt to untrash the workflow
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.untrash_object(type_and_id, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // check that object is still in trash
        assert_trashed_status_for_object(&mut app, &sync_id.uid(), true);
        assert_pending_status_for_object(&mut app, &sync_id.uid(), false);

        // We do not optimistically update trashed_ts when untrashing, so there should be no event.
        assert!(cloud_events(&update_manager_struct).is_empty());
    })
}

#[test]
fn test_metadata_after_optimistic_grab_baton_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());

        let initial_ts = Utc::now();

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: initial_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        let notebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        // Mock a successful baton grab.
        let mut grab_metadata = notebook_metadata.clone();
        grab_metadata.current_editor_uid = Some(TEST_USER_UID.to_string());
        grab_metadata.metadata_last_updated_ts = (initial_ts + chrono::Duration::seconds(1)).into();
        server_api
            .expect_grab_notebook_edit_access()
            .times(1)
            .return_once(move |_| Ok(grab_metadata));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Grab the baton optimistically.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.grab_notebook_edit_access(sync_id, true, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_current_editor_for_object(&mut app, &sync_id.uid(), Some(TEST_USER_UID));
        // Ensure that the metadata timestamp from the server response is used.
        assert_metadata_ts_for_object(
            &mut app,
            &sync_id.uid(),
            (initial_ts + chrono::Duration::seconds(1)).into(),
        );

        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ModelEvent::UpdateObjectMetadata { id, metadata } => {
                assert_eq!(id, &sync_id.sqlite_uid_hash(ObjectIdType::Notebook));
                assert_eq!(metadata.current_editor_uid.as_deref(), Some(TEST_USER_UID));
            }
            _ => panic!("Expected an UpdateObjectMetadata event"),
        }
    });
}

#[test]
fn test_metadata_after_optimistic_grab_baton_failure() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());

        let initial_ts = Utc::now();

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: initial_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        let notebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        // Mock a failed baton grab.
        server_api
            .expect_grab_notebook_edit_access()
            .times(1)
            .return_once(move |_| Err(anyhow::anyhow!("Baton grab failed")));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Grab the baton optimistically.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.grab_notebook_edit_access(sync_id, true, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // On failure, the editor is still updated, but the metadata timestamp is not - future
        // metadata updates would override it.
        assert_current_editor_for_object(&mut app, &sync_id.uid(), Some(TEST_USER_UID));
        assert_metadata_ts_for_object(&mut app, &sync_id.uid(), initial_ts.into());

        let events = db_events(&update_manager_struct);
        assert!(events.is_empty());
    });
}

#[test]
fn test_metadata_after_non_optimistic_grab_baton_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());

        let initial_ts = Utc::now();

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: initial_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        let notebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        // Mock a successful baton grab.
        let mut grab_metadata = notebook_metadata.clone();
        grab_metadata.current_editor_uid = Some(TEST_USER_UID.to_string());
        grab_metadata.metadata_last_updated_ts = (initial_ts + chrono::Duration::seconds(1)).into();
        server_api
            .expect_grab_notebook_edit_access()
            .times(1)
            .return_once(move |_| Ok(grab_metadata));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Grab the baton optimistically.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.grab_notebook_edit_access(sync_id, false, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_current_editor_for_object(&mut app, &sync_id.uid(), Some(TEST_USER_UID));
        // Ensure that the metadata timestamp from the server response is used.
        assert_metadata_ts_for_object(
            &mut app,
            &sync_id.uid(),
            (initial_ts + chrono::Duration::seconds(1)).into(),
        );

        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ModelEvent::UpdateObjectMetadata { id, metadata } => {
                assert_eq!(id, &sync_id.sqlite_uid_hash(ObjectIdType::Notebook));
                assert_eq!(metadata.current_editor_uid.as_deref(), Some(TEST_USER_UID));
            }
            _ => panic!("Expected an UpdateObjectMetadata event"),
        }
    });
}

#[test]
fn test_metadata_after_non_optimistic_grab_baton_failure() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());

        let initial_ts = Utc::now();

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: initial_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        let notebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        // Mock a failed baton grab.
        server_api
            .expect_grab_notebook_edit_access()
            .times(1)
            .return_once(move |_| Err(anyhow::anyhow!("Baton grab failed")));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Grab the baton non-optimistically.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.grab_notebook_edit_access(sync_id, false, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // On failure, neither the editor nor the metadata timestamp are updated.
        assert_current_editor_for_object(&mut app, &sync_id.uid(), None);
        assert_metadata_ts_for_object(&mut app, &sync_id.uid(), initial_ts.into());

        let events = db_events(&update_manager_struct);
        assert!(events.is_empty());
    });
}

#[test]
fn test_report_initial_load() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();
        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Before the initial load, the listener should be pending.
        let mut listener = Box::pin(
            update_manager_struct
                .update_manager
                .read(&app, |update_manager, _| {
                    update_manager.initial_load_complete()
                }),
        );
        assert!(future::poll_once(&mut listener).await.is_none());

        receive_initial_load_or_polling_update(
            &mut app,
            &update_manager_struct.update_manager,
            false, /* force_refresh */
            InitialLoadResponse {
                updated_notebooks: Default::default(),
                deleted_notebooks: Default::default(),
                updated_workflows: Default::default(),
                deleted_workflows: Default::default(),
                updated_folders: Default::default(),
                deleted_folders: Default::default(),
                user_profiles: Default::default(),
                updated_generic_string_objects: Default::default(),
                deleted_generic_string_objects: Default::default(),
                action_histories: Default::default(),
                mcp_gallery: Default::default(),
            },
        );

        // Afterwards, the listener should get notified.
        assert!(future::poll_once(listener).await.is_some());

        // Subsequent listeners should complete immediately.
        let listener = update_manager_struct
            .update_manager
            .read(&app, |update_manager, _| {
                update_manager.initial_load_complete()
            });
        assert!(future::poll_once(listener).await.is_some());
    });
}

#[test]
fn test_get_duplicate_object_name() {
    assert_eq!(
        get_duplicate_object_name("my object name"),
        "my object name (1)"
    );
    assert_eq!(
        get_duplicate_object_name("my object name (1)"),
        "my object name (2)"
    );
    assert_eq!(
        get_duplicate_object_name("my object name (23)"),
        "my object name (24)"
    );
    assert_eq!(
        get_duplicate_object_name("my object name(1234)"),
        "my object name(1234) (1)"
    );
    assert_eq!(
        get_duplicate_object_name("my object name (0)"),
        "my object name (1)"
    );
    assert_eq!(
        get_duplicate_object_name("my object name (-3)"),
        "my object name (-3) (1)"
    );
    assert_eq!(
        get_duplicate_object_name("my object name (18446744073709551615)"),
        "my object name (18446744073709551615) (1)"
    );
    assert_eq!(
        get_duplicate_object_name("my object name (18446744073709551616)"),
        "my object name (18446744073709551616) (1)"
    );
}

#[test]
fn test_accepts_new_metadata_with_force_refresh() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();
        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id: SyncId = SyncId::ServerId(notebook_id.into());

        let current_metadata_ts = Utc::now();
        let metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook =
            mock_server_notebook(notebook_id, Owner::mock_current_user(), metadata);

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        let mocked_metadata = ServerMetadata {
            uid: server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: Some("BoogaBooga".to_string()),
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let mocked_response = InitialLoadResponse {
            updated_notebooks: vec![ServerNotebook::new(
                SyncId::ServerId(server_id),
                CloudNotebookModel {
                    title: "".into(),
                    data: "".into(),
                    ai_document_id: None,
                    conversation_id: None,
                },
                mocked_metadata.clone(),
                mock_server_permissions(Owner::mock_current_user()),
            )],
            deleted_notebooks: vec![],
            updated_workflows: vec![],
            deleted_workflows: vec![],
            updated_folders: vec![],
            deleted_folders: vec![],
            user_profiles: vec![],
            updated_generic_string_objects: Default::default(),
            deleted_generic_string_objects: Default::default(),
            action_histories: Default::default(),
            mcp_gallery: Default::default(),
        };

        // Force a sync for all objects
        receive_initial_load_or_polling_update(
            &mut app,
            &update_manager_struct.update_manager,
            true, /* force_refresh */
            mocked_response,
        );

        // Assert that the metadata changes are correctly applied
        CloudModel::handle(&app).read(&app, |cloud_model, _ctx| {
            if let Some(object) = cloud_model.get_by_uid(&notebook_id.to_server_id().uid()) {
                assert_eq!(object.metadata().creator_uid, mocked_metadata.creator_uid);
            } else {
                panic!("object should have been in cloud model, but wasn't");
            }
        });

        let events = db_events(&update_manager_struct);

        // All the upserts from polling
        assert!(matches!(&events[0], ModelEvent::SyncObjectActions { .. }));
        assert!(matches!(&events[1], ModelEvent::UpsertNotebooks(_)));
        assert!(matches!(&events[2], ModelEvent::UpsertWorkflows(_)));
        assert!(matches!(&events[3], ModelEvent::UpsertFolders(_)));
        assert!(matches!(&events[4], ModelEvent::DeleteObjects { ids: _ }));
    });
}

#[test]
fn test_delete_single_object() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        // Create notebook object
        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());
        let ts = Utc::now();

        let server_metadata = ServerMetadata {
            uid: server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: ts.into(),
            trashed_ts: Some(ts.into()),
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let server_notebook =
            mock_server_notebook(notebook_id, Owner::mock_current_user(), server_metadata);
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(
                sync_id,
                CloudNotebook::new_from_server(server_notebook.clone()),
            );
        });

        // Mock delete
        server_api
            .expect_delete_object()
            .times(1)
            .return_once(move |_| {
                Ok(ObjectDeleteResult::Success {
                    deleted_ids: vec![SyncId::ServerId(notebook_id.into())],
                })
            });

        // Delete notebook
        let type_and_id = CloudObjectTypeAndId::Notebook(SyncId::ServerId(notebook_id.into()));
        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.delete_object_by_user(type_and_id, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // Check that notebook is not in CloudModel anymore
        CloudModel::handle(&app).update(&mut app, |cloud_model, _ctx| {
            assert!(
                !cloud_model.check_if_object_is_in_cloudmodel(notebook_id.to_server_id().uid()),
                "Deleted object should not be in CloudModel anymore"
            );
        });

        // Check that we reported the deletion.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectDeleted {
                type_and_id,
                folder_id: None
            }]
        );
    });
}

#[test]
fn test_empty_trash() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let ts = Utc::now();

        // Create a few objects in the Personal space
        let notebook_server_id: ServerId = 123.into();
        let notebook_id: NotebookId = notebook_server_id.into();
        let notebook_sync_id = SyncId::ServerId(notebook_id.into());
        let notebook_metadata = ServerMetadata {
            uid: notebook_server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: ts.into(),
            trashed_ts: Some(ts.into()),
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook =
            mock_server_notebook(notebook_id, Owner::mock_current_user(), notebook_metadata);

        let workflow_server_id: ServerId = 456.into();
        let workflow_id: WorkflowId = workflow_server_id.into();
        let workflow_sync_id = SyncId::ServerId(workflow_id.into());
        let workflow_metadata = ServerMetadata {
            uid: workflow_server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: ts.into(),
            trashed_ts: Some(ts.into()),
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let workflow =
            mock_server_workflow(workflow_id, Owner::mock_current_user(), workflow_metadata);

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(
                notebook_sync_id,
                CloudNotebook::new_from_server(notebook.clone()),
            );
            cloud_model.add_object(
                workflow_sync_id,
                CloudWorkflow::new_from_server(workflow.clone()),
            );
        });

        // Mock delete
        server_api
            .expect_empty_trash()
            .times(1)
            .return_once(move |_| {
                Ok(ObjectDeleteResult::Success {
                    deleted_ids: vec![
                        SyncId::ServerId(workflow_id.into()),
                        SyncId::ServerId(notebook_id.into()),
                    ],
                })
            });

        // Empty trash
        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.empty_trash(Space::Personal, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // Check that notebook is not in CloudModel anymore
        CloudModel::handle(&app).update(&mut app, |cloud_model, _ctx| {
            assert!(
                !cloud_model.check_if_object_is_in_cloudmodel(notebook_id.to_server_id().uid()),
                "Deleted notebook should not be in CloudModel anymore"
            );
            assert!(
                !cloud_model.check_if_object_is_in_cloudmodel(workflow_id.to_server_id().uid()),
                "Deleted workflow should not be in CloudModel anymore"
            );
        });

        // Check that we reported the deletions.

        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![
                CloudModelEvent::ObjectDeleted {
                    type_and_id: CloudObjectTypeAndId::from_id_and_type(
                        workflow_sync_id,
                        ObjectType::Workflow
                    ),
                    folder_id: None
                },
                CloudModelEvent::ObjectDeleted {
                    type_and_id: CloudObjectTypeAndId::from_id_and_type(
                        notebook_sync_id,
                        ObjectType::Notebook
                    ),
                    folder_id: None
                },
            ]
        );
    });
}

#[test]
fn test_leave_shared_object() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let sharing_team = Owner::Team {
            team_uid: 99.into(),
        };

        // Create a folder.
        let folder_server_id: ServerId = 456.into();
        let folder_id: FolderId = folder_server_id.into();
        let folder_sync_id = SyncId::ServerId(folder_server_id);
        let server_folder = mock_server_folder(
            folder_id,
            sharing_team,
            ServerMetadata {
                uid: folder_server_id,
                revision: Revision::now(),
                metadata_last_updated_ts: Utc::now().into(),
                trashed_ts: None,
                folder_id: None,
                is_welcome_object: false,
                creator_uid: None,
                last_editor_uid: None,
                current_editor_uid: None,
            },
        );

        // Create a notebook in the folder.
        let notebook_server_id: ServerId = 123.into();
        let notebook_id: NotebookId = notebook_server_id.into();
        let notebook_sync_id = SyncId::ServerId(notebook_server_id);

        let server_notebook = mock_server_notebook(
            notebook_id,
            sharing_team,
            ServerMetadata {
                uid: notebook_server_id,
                revision: Revision::now(),
                metadata_last_updated_ts: Utc::now().into(),
                trashed_ts: None,
                folder_id: Some(folder_id),
                is_welcome_object: false,
                creator_uid: None,
                last_editor_uid: None,
                current_editor_uid: None,
            },
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(folder_sync_id, CloudFolder::new_from_server(server_folder));
            cloud_model.add_object(
                notebook_sync_id,
                CloudNotebook::new_from_server(server_notebook),
            );
        });

        // Mock leaving the folder.
        server_api
            .expect_leave_object()
            .times(1)
            .return_once(move |_| {
                Ok(ObjectDeleteResult::Success {
                    deleted_ids: vec![folder_sync_id],
                })
            });

        // Leave the folder.
        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.leave_object(folder_server_id, ctx);
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // Check that neither object is in CloudModel.
        CloudModel::handle(&app).update(&mut app, |cloud_model, _ctx| {
            assert!(
                !cloud_model.check_if_object_is_in_cloudmodel(folder_server_id.uid()),
                "Left object should not be in CloudModel anymore"
            );
            assert!(
                !cloud_model.check_if_object_is_in_cloudmodel(notebook_server_id.uid()),
                "Left object contents should not be in CloudModel anymore"
            );
        });

        // Check that we reported the deleted objects.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![
                CloudModelEvent::ObjectDeleted {
                    type_and_id: CloudObjectTypeAndId::Folder(folder_sync_id),
                    folder_id: None
                },
                CloudModelEvent::ObjectDeleted {
                    type_and_id: CloudObjectTypeAndId::Notebook(notebook_sync_id),
                    folder_id: Some(folder_sync_id)
                }
            ]
        );
    });
}

#[test]
fn test_create_object_online_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let client_id = ClientId::new();
        let workflow_id: WorkflowId = 123.into();
        let server_id = workflow_id.to_server_id();
        let sync_id = SyncId::ServerId(server_id);

        // Create known timestamps for assertions
        let revision = Revision::now();
        let metadata_ts = Utc::now().into();
        let last_editor_uid = "34jkaosdfj".to_string();

        // Clone for use in the closure
        let revision_clone = revision;
        let metadata_ts_clone = metadata_ts;

        server_api
            .expect_create_workflow()
            .times(1)
            .return_once(move |_| {
                Ok(CreateCloudObjectResult::Success {
                    created_cloud_object: CreatedCloudObject {
                        client_id,
                        revision_and_editor: RevisionAndLastEditor {
                            revision: revision_clone,
                            last_editor_uid: Some(last_editor_uid.clone()),
                        },
                        metadata_ts: metadata_ts_clone,
                        server_id_and_type: ServerIdAndType {
                            id: workflow_id.to_server_id(),
                            id_type: ObjectIdType::Workflow,
                        },
                        creator_uid: None,
                        permissions: ServerPermissions::mock_personal(),
                    },
                })
            });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Create an online object, bypassing the sync queue.
        let result =
            update_manager_struct
                .update_manager
                .update(&mut app, |update_manager, ctx| {
                    update_manager.create_object_online(
                        CloudWorkflowModel::new(Workflow::new(
                            "test workflow".to_owned(),
                            "echo test".to_owned(),
                        )),
                        Owner::mock_current_user(),
                        client_id,
                        CloudObjectEventEntrypoint::Unknown,
                        false,
                        None,
                        ctx,
                    )
                });

        // Wait for the operation to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // Await the result
        let returned_server_id = result.await.expect("create should succeed");
        assert_eq!(returned_server_id, server_id);

        // Verify the object was created in CloudModel.
        CloudModel::handle(&app).read(&app, |cloud_model, _ctx| {
            let object = cloud_model
                .get_workflow(&sync_id)
                .expect("workflow should exist in cloud model");
            assert_eq!(object.model().data.name(), "test workflow");
            assert_eq!(object.model().data.command(), Some("echo test"));
            assert!(!object.metadata.has_pending_content_changes());
            assert!(!object.metadata.has_pending_online_only_change());
        });

        // Verify metadata and revision timestamps are set correctly
        assert_revision_for_object(&app, &sync_id.uid(), revision);
        assert_metadata_ts_for_object(&mut app, &sync_id.uid(), metadata_ts);

        // Verify database events
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            ModelEvent::UpsertWorkflow { workflow: _ }
        ));
    })
}

#[test]
fn test_create_object_online_failure() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let client_id = ClientId::new();

        server_api
            .expect_create_workflow()
            .times(1)
            .returning(move |_| Err(anyhow::anyhow!("Server error")));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Create the object.
        let result =
            update_manager_struct
                .update_manager
                .update(&mut app, |update_manager, ctx| {
                    update_manager.create_object_online(
                        CloudWorkflowModel::new(Workflow::new(
                            "test workflow".to_owned(),
                            "echo test".to_owned(),
                        )),
                        Owner::mock_current_user(),
                        client_id,
                        CloudObjectEventEntrypoint::Unknown,
                        false,
                        None,
                        ctx,
                    )
                });

        // Wait for the operation to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // Await the result and expect failure.
        let error = result.await.expect_err("create should fail");
        assert_eq!(error.to_string(), "Server error");

        // Verify the object was NOT created in CloudModel.
        CloudModel::handle(&app).read(&app, |cloud_model, _ctx| {
            assert!(
                cloud_model
                    .get_workflow(&SyncId::ClientId(client_id))
                    .is_none()
            );
        });

        // Verify no database events occurred.
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 0);
    })
}

#[test]
fn test_create_object_online_user_facing_error() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let client_id = ClientId::new();

        server_api
            .expect_create_workflow()
            .times(1)
            .return_once(move |_| {
                Ok(CreateCloudObjectResult::UserFacingError(
                    "You have reached your limit".to_string(),
                ))
            });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Create the object.
        let result =
            update_manager_struct
                .update_manager
                .update(&mut app, |update_manager, ctx| {
                    update_manager.create_object_online(
                        CloudWorkflowModel::new(Workflow::new(
                            "test workflow".to_owned(),
                            "echo test".to_owned(),
                        )),
                        Owner::mock_current_user(),
                        client_id,
                        CloudObjectEventEntrypoint::Unknown,
                        false,
                        None,
                        ctx,
                    )
                });

        // Wait for the operation to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // Await the result and expect failure.
        let error = result.await.expect_err("create should fail");
        assert_eq!(error.to_string(), "You have reached your limit");

        // Verify the object was NOT created in CloudModel.
        CloudModel::handle(&app).read(&app, |cloud_model, _ctx| {
            assert!(
                cloud_model
                    .get_workflow(&SyncId::ClientId(client_id))
                    .is_none()
            );
        });

        // Verify no database events occurred.
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 0);
    })
}

#[test]
fn test_create_object_online_with_folder_id() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let client_id = ClientId::new();
        let workflow_id: WorkflowId = 123.into();
        let server_id = workflow_id.to_server_id();
        let folder_id: FolderId = 456.into();
        let folder_sync_id = SyncId::ServerId(folder_id.into());
        let sync_id = SyncId::ServerId(server_id);

        // Create a folder in cloud model first.
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            add_folder(folder_id, Owner::mock_current_user(), cloud_model);
        });

        server_api
            .expect_create_workflow()
            .times(1)
            .return_once(move |request| {
                // Verify the folder ID was passed through.
                assert_eq!(
                    request.initial_folder_id,
                    Some(FolderId::from(folder_id.to_server_id()))
                );
                Ok(CreateCloudObjectResult::Success {
                    created_cloud_object: CreatedCloudObject {
                        client_id,
                        revision_and_editor: RevisionAndLastEditor {
                            revision: Revision::now(),
                            last_editor_uid: Some("34jkaosdfj".to_string()),
                        },
                        metadata_ts: DateTime::<Utc>::default().into(),
                        server_id_and_type: ServerIdAndType {
                            id: workflow_id.to_server_id(),
                            id_type: ObjectIdType::Workflow,
                        },
                        creator_uid: None,
                        permissions: ServerPermissions::mock_personal(),
                    },
                })
            });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Call create_object_online with a folder ID.
        let result =
            update_manager_struct
                .update_manager
                .update(&mut app, |update_manager, ctx| {
                    update_manager.create_object_online(
                        CloudWorkflowModel::new(Workflow::new(
                            "test workflow".to_owned(),
                            "echo test".to_owned(),
                        )),
                        Owner::mock_current_user(),
                        client_id,
                        CloudObjectEventEntrypoint::Unknown,
                        false,
                        Some(folder_sync_id),
                        ctx,
                    )
                });

        // Wait for the operation to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // Await the result.
        let returned_server_id = result.await.expect("create should succeed");
        assert_eq!(returned_server_id, server_id);

        // Verify the object was created in CloudModel with the correct folder.
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_sync_id));
    })
}

#[test]
fn test_create_object_online_with_client_folder_id_fails() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let client_id = ClientId::new();
        let folder_client_id = ClientId::new();

        // Don't expect any API calls since the function should return early.
        server_api.expect_create_workflow().times(0);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Call create_object_online with a client folder ID (which should fail).
        let result =
            update_manager_struct
                .update_manager
                .update(&mut app, |update_manager, ctx| {
                    update_manager.create_object_online(
                        CloudWorkflowModel::new(Workflow::new(
                            "test workflow".to_owned(),
                            "echo test".to_owned(),
                        )),
                        Owner::mock_current_user(),
                        client_id,
                        CloudObjectEventEntrypoint::Unknown,
                        false,
                        Some(SyncId::ClientId(folder_client_id)),
                        ctx,
                    )
                });

        // Await the result and expect failure.
        let error = result.await.expect_err("create should fail");
        assert!(
            error
                .to_string()
                .contains("Folder does not exist on the server")
        );

        // Verify the object was NOT created in CloudModel.
        CloudModel::handle(&app).read(&app, |cloud_model, _ctx| {
            assert!(
                cloud_model
                    .get_workflow(&SyncId::ClientId(client_id))
                    .is_none()
            );
        });

        // Verify no database events occurred.
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 0);
    })
}

#[test]
fn test_overwrite_object_action_history_no_actions_on_client() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();

        let timestamp = Utc::now();

        let hashed_object_id = "Workflow-asdf".to_string();
        let hashed_object_id_clone = hashed_object_id.clone();

        let actions: Vec<ObjectAction> = vec![
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp,
                    processed_at_timestamp: Some(timestamp),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(10),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(10)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::BundledActions {
                    count: 5,
                    oldest_timestamp: timestamp - chrono::Duration::minutes(35),
                    latest_timestamp: timestamp - chrono::Duration::minutes(15),
                    latest_processed_at_timestamp: timestamp - chrono::Duration::minutes(15),
                },
            },
        ];

        let mock_history = ObjectActionHistory {
            uid: hashed_object_id.clone(),
            hashed_sqlite_id: hashed_object_id.clone(),
            latest_processed_at_timestamp: timestamp,
            actions,
        };

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.maybe_overwrite_object_action_history(&mock_history, ctx);
            });

        // Check that these actions are accepted
        ObjectActions::handle(&app).update(&mut app, |model, _ctx| {
            assert_eq!(model.count_actions_for_object(&hashed_object_id_clone), 3);
        });
    });
}

#[test]
fn test_overwrite_object_action_history_reject() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();

        let timestamp = Utc::now();

        let hashed_object_id = "Workflow-asdf".to_string();
        let hashed_object_id_clone = hashed_object_id.clone();

        // the server's most recent action was 1 minute ago
        let server_actions: Vec<ObjectAction> = vec![
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(1),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(1)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(10),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(10)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(12),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(12)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::BundledActions {
                    count: 5,
                    oldest_timestamp: timestamp - chrono::Duration::minutes(35),
                    latest_timestamp: timestamp - chrono::Duration::minutes(15),
                    latest_processed_at_timestamp: timestamp - chrono::Duration::minutes(15),
                },
            },
        ];

        // The client actions have one action that is more recent than what the server is sending.
        let client_actions: Vec<ObjectAction> = vec![
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp,
                    processed_at_timestamp: Some(timestamp),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(1),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(1)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(10),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(10)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(12),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(12)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::BundledActions {
                    count: 5,
                    oldest_timestamp: timestamp - chrono::Duration::minutes(35),
                    latest_timestamp: timestamp - chrono::Duration::minutes(15),
                    latest_processed_at_timestamp: timestamp - chrono::Duration::minutes(15),
                },
            },
        ];

        let mock_history = ObjectActionHistory {
            uid: hashed_object_id.clone(),
            hashed_sqlite_id: hashed_object_id.clone(),
            latest_processed_at_timestamp: timestamp - chrono::Duration::minutes(1),
            actions: server_actions,
        };

        // We should have 0 actions for this object
        ObjectActions::handle(&app).update(&mut app, |model, _ctx| {
            assert_eq!(model.count_actions_for_object(&hashed_object_id_clone), 0);
        });

        // Now manually overwrite the data for this object
        ObjectActions::handle(&app).update(&mut app, |model, ctx| {
            model.overwrite_action_history_for_object(&hashed_object_id, client_actions, ctx)
        });

        // We should have 5 actions for this object
        ObjectActions::handle(&app).update(&mut app, |model, _ctx| {
            assert_eq!(model.count_actions_for_object(&hashed_object_id_clone), 5);
        });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.maybe_overwrite_object_action_history(&mock_history, ctx);
            });

        // Check that the new actions were rejected, and we still have 5 actions
        ObjectActions::handle(&app).update(&mut app, |model, _ctx| {
            assert_eq!(model.count_actions_for_object(&hashed_object_id_clone), 5);
        });
    });
}

#[test]
fn test_overwrite_object_action_history_ignores_pending_local_actions() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();

        let timestamp = Utc::now();

        let hashed_object_id = "Workflow-asdf".to_string();
        let hashed_object_id_clone = hashed_object_id.clone();

        let server_actions: Vec<ObjectAction> = vec![
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(1),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(1)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(10),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(10)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(12),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(12)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::BundledActions {
                    count: 5,
                    oldest_timestamp: timestamp - chrono::Duration::minutes(35),
                    latest_timestamp: timestamp - chrono::Duration::minutes(15),
                    latest_processed_at_timestamp: timestamp - chrono::Duration::minutes(15),
                },
            },
        ];

        // The client actions have one action that is more recent than what the server is sending.
        let client_actions: Vec<ObjectAction> = vec![
            // This action is pending so we should still accept the new
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp,
                    processed_at_timestamp: Some(timestamp),
                    data: None,
                    pending: true,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(2),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(2)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(10),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(10)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(12),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(12)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp - chrono::Duration::minutes(13),
                    processed_at_timestamp: Some(timestamp - chrono::Duration::minutes(13)),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                uid: hashed_object_id.clone(),
                hashed_sqlite_id: hashed_object_id.clone(),
                action_type: ObjectActionType::Execute,
                action_subtype: ObjectActionSubtype::BundledActions {
                    count: 5,
                    oldest_timestamp: timestamp - chrono::Duration::minutes(35),
                    latest_timestamp: timestamp - chrono::Duration::minutes(15),
                    latest_processed_at_timestamp: timestamp - chrono::Duration::minutes(15),
                },
            },
        ];

        let mock_history = ObjectActionHistory {
            uid: hashed_object_id.clone(),
            hashed_sqlite_id: hashed_object_id.clone(),
            latest_processed_at_timestamp: timestamp - chrono::Duration::minutes(1),
            actions: server_actions,
        };

        ObjectActions::handle(&app).update(&mut app, |model, ctx| {
            model.overwrite_action_history_for_object(&hashed_object_id, client_actions, ctx)
        });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.maybe_overwrite_object_action_history(&mock_history, ctx);
            });

        // The new actions should be accepted, but the pending action should be persisted as well.
        ObjectActions::handle(&app).update(&mut app, |model, _ctx| {
            assert_eq!(model.count_actions_for_object(&hashed_object_id_clone), 5);
        });
    });
}

#[test]
fn test_object_action_histories_with_initial_load() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();

        let workflow_id_a: SyncId =
            SyncId::ServerId(ServerId::from_string_lossy("JLKS23FSJLKS23FSJLKS23"));
        let workflow_id_b: SyncId =
            SyncId::ServerId(ServerId::from_string_lossy("LSJLK23fZDLSJLK23fZDLS"));
        let workflow_id_c: SyncId =
            SyncId::ServerId(ServerId::from_string_lossy("SDFlJ23SDfSDFlJ23SDfSD"));

        let timestamp = Utc::now();
        let timestamp_old = Utc::now() - chrono::Duration::seconds(1);
        let timestamp_older = Utc::now() - chrono::Duration::seconds(2);
        let timestamp_oldest = Utc::now() - chrono::Duration::seconds(3);

        let actions_a = vec![ObjectAction {
            action_type: ObjectActionType::Execute,
            uid: workflow_id_a.uid(),
            hashed_sqlite_id: workflow_id_a.uid(),
            action_subtype: ObjectActionSubtype::SingleAction {
                timestamp: timestamp_old,
                processed_at_timestamp: Some(timestamp_old),
                data: None,
                pending: false,
            },
        }];
        let actions_b = vec![ObjectAction {
            action_type: ObjectActionType::Execute,
            uid: workflow_id_a.uid(),
            hashed_sqlite_id: workflow_id_a.uid(),
            action_subtype: ObjectActionSubtype::SingleAction {
                timestamp: timestamp_older,
                processed_at_timestamp: Some(timestamp_older),
                data: None,
                pending: false,
            },
        }];

        ObjectActions::handle(&app).update(&mut app, |object_actions, ctx| {
            object_actions.overwrite_action_history_for_object(
                &workflow_id_a.uid(),
                actions_a,
                ctx,
            );

            object_actions.overwrite_action_history_for_object(
                &workflow_id_b.uid(),
                actions_b,
                ctx,
            );
        });

        let actions_a_server = vec![
            ObjectAction {
                action_type: ObjectActionType::Execute,
                uid: workflow_id_a.uid(),
                hashed_sqlite_id: workflow_id_a.uid(),
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp,
                    processed_at_timestamp: Some(timestamp),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                action_type: ObjectActionType::Execute,
                uid: workflow_id_a.uid(),
                hashed_sqlite_id: workflow_id_a.uid(),
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp_old,
                    processed_at_timestamp: Some(timestamp_old),
                    data: None,
                    pending: false,
                },
            },
        ];

        let actions_b_server = vec![
            ObjectAction {
                action_type: ObjectActionType::Execute,
                uid: workflow_id_a.uid(),
                hashed_sqlite_id: workflow_id_a.uid(),
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp_old,
                    processed_at_timestamp: Some(timestamp_old),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                action_type: ObjectActionType::Execute,
                uid: workflow_id_a.uid(),
                hashed_sqlite_id: workflow_id_a.uid(),
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp: timestamp_older,
                    processed_at_timestamp: Some(timestamp_older),
                    data: None,
                    pending: false,
                },
            },
        ];

        let actions_c_server = vec![ObjectAction {
            action_type: ObjectActionType::Execute,
            uid: workflow_id_a.uid(),
            hashed_sqlite_id: workflow_id_a.uid(),
            action_subtype: ObjectActionSubtype::SingleAction {
                timestamp: timestamp_oldest,
                processed_at_timestamp: Some(timestamp_oldest),
                data: None,
                pending: false,
            },
        }];

        let server_action_histories = vec![
            ObjectActionHistory {
                uid: workflow_id_a.uid(),
                hashed_sqlite_id: workflow_id_a.uid(),
                latest_processed_at_timestamp: timestamp,
                actions: actions_a_server,
            },
            ObjectActionHistory {
                uid: workflow_id_b.uid(),
                hashed_sqlite_id: workflow_id_b.uid(),
                latest_processed_at_timestamp: timestamp_old,
                actions: actions_b_server,
            },
            ObjectActionHistory {
                uid: workflow_id_c.uid(),
                hashed_sqlite_id: workflow_id_c.uid(),
                latest_processed_at_timestamp: timestamp_oldest,
                actions: actions_c_server,
            },
        ];
        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        let mocked_response = InitialLoadResponse {
            updated_notebooks: vec![],
            deleted_notebooks: vec![],
            updated_workflows: vec![],
            deleted_workflows: vec![],
            updated_folders: vec![],
            deleted_folders: vec![],
            user_profiles: vec![],
            updated_generic_string_objects: Default::default(),
            deleted_generic_string_objects: Default::default(),
            action_histories: server_action_histories,
            mcp_gallery: Default::default(),
        };
        receive_initial_load_or_polling_update(
            &mut app,
            &update_manager_struct.update_manager,
            false, /* force_refresh */
            mocked_response,
        );

        // Assert new ObjectAction state
        ObjectActions::handle(&app).update(&mut app, |object_actions, _| {
            assert_eq!(
                object_actions.count_actions_for_object(&workflow_id_a.uid()),
                2
            );
            assert_eq!(
                object_actions.count_actions_for_object(&workflow_id_b.uid()),
                2
            );
            assert_eq!(
                object_actions.count_actions_for_object(&workflow_id_c.uid()),
                1
            );
        });

        let events = db_events(&update_manager_struct);

        // All the upserts from polling
        assert!(matches!(&events[0], ModelEvent::SyncObjectActions { .. }));
        assert!(matches!(&events[1], ModelEvent::UpsertNotebooks(_)));
        assert!(matches!(&events[2], ModelEvent::UpsertWorkflows(_)));
        assert!(matches!(&events[3], ModelEvent::UpsertFolders(_)));
        assert!(matches!(&events[4], ModelEvent::DeleteObjects { ids: _ }));
    });
}

#[test]
fn test_delete_single_object_with_actions() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        // Create workflow object
        let server_id: ServerId = 123.into();
        let workflow_id: WorkflowId = server_id.into();
        let sync_id = SyncId::ServerId(workflow_id.into());
        let ts = Utc::now();

        let server_metadata = ServerMetadata {
            uid: server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: ts.into(),
            trashed_ts: Some(ts.into()),
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let server_workflow =
            mock_server_workflow(workflow_id, Owner::mock_current_user(), server_metadata);
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(
                sync_id,
                CloudWorkflow::new_from_server(server_workflow.clone()),
            );
        });

        let timestamp = Utc::now();
        // Add some actions to this object
        let actions_on_object = vec![
            ObjectAction {
                action_type: ObjectActionType::Execute,
                uid: SyncId::ServerId(workflow_id.into()).uid(),
                hashed_sqlite_id: SyncId::ServerId(workflow_id.into()).uid(),
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp,
                    processed_at_timestamp: Some(timestamp),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                action_type: ObjectActionType::Execute,
                uid: SyncId::ServerId(workflow_id.into()).uid(),
                hashed_sqlite_id: SyncId::ServerId(workflow_id.into()).uid(),
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp,
                    processed_at_timestamp: Some(timestamp),
                    data: None,
                    pending: false,
                },
            },
            ObjectAction {
                action_type: ObjectActionType::Execute,
                uid: SyncId::ServerId(workflow_id.into()).uid(),
                hashed_sqlite_id: SyncId::ServerId(workflow_id.into()).uid(),
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp,
                    processed_at_timestamp: Some(timestamp),
                    data: None,
                    pending: false,
                },
            },
        ];
        ObjectActions::handle(&app).update(&mut app, |object_actions, ctx| {
            object_actions.overwrite_action_history_for_object(
                &SyncId::ServerId(workflow_id.into()).uid(),
                actions_on_object,
                ctx,
            )
        });

        // Mock delete
        server_api
            .expect_delete_object()
            .times(1)
            .return_once(move |_| {
                Ok(ObjectDeleteResult::Success {
                    deleted_ids: vec![SyncId::ServerId(workflow_id.into())],
                })
            });

        // Delete workflow
        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.delete_object_by_user(
                    CloudObjectTypeAndId::Workflow(SyncId::ServerId(workflow_id.into())),
                    ctx,
                );
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // Check that workflow is not in CloudModel anymore
        CloudModel::handle(&app).update(&mut app, |cloud_model, _ctx| {
            assert!(
                !cloud_model.check_if_object_is_in_cloudmodel(workflow_id.to_server_id().uid()),
                "Deleted object should not be in CloudModel anymore"
            );
        });

        // Ensure the actions are also deleted.
        ObjectActions::handle(&app).update(&mut app, |object_actions, _| {
            assert_eq!(
                object_actions
                    .count_actions_for_object(&SyncId::ServerId(workflow_id.into()).uid()),
                0
            );
        });

        let events = db_events(&update_manager_struct);
        assert!(matches!(&events[0], ModelEvent::DeleteObjects { .. }));
    });
}

/// Test successfully moving an object from a user's personal space to a team drive. This covers
/// changing the owner of an object, which implicitly also clears its parent folder.
#[test]
fn test_move_object_personal_to_team_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());
        let folder_id: FolderId = 456.into();
        let team = Space::Team {
            team_uid: ServerId::from(789),
        };

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: Some(folder_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
            add_folder(folder_id, Owner::mock_current_user(), cloud_model);
        });

        server_api
            .expect_transfer_notebook_owner()
            .times(1)
            .return_once(move |_, _| Ok(true));

        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));
        assert_space_for_object(&app, &sync_id.uid(), Space::Personal);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Move the notebook from a personal folder to the team space.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Notebook);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Space(team),
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), true);
        assert_space_for_object(&app, &sync_id.uid(), team);
        // Currently, we can only move to the root of another space.
        assert_root_level_for_object(&mut app, &sync_id.uid(), true);

        // There should be an optimistic model event, but no database write.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: Some(folder_id.into()),
                to_folder: None,
            }]
        );
        assert!(db_events(&update_manager_struct).is_empty());

        // Wait for the move to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), false);
        assert_root_level_for_object(&mut app, &sync_id.uid(), true);
        assert_space_for_object(&app, &sync_id.uid(), team);

        // After the move succeeds on the server, we update the DB, but don't emit another model
        // event.
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], ModelEvent::UpsertNotebook { notebook } if notebook.id == sync_id),
            "Expected upsert of notebook, got {:?}",
            &events[0]
        );
    });
}

/// Test failing to move an object from a user's personal space to a team drive. This tests that we
/// optimistically update the model, and then roll those updates back on failure.
#[test]
fn test_move_object_personal_to_team_failure() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());
        let folder_id: FolderId = 456.into();
        let team = Space::Team {
            team_uid: ServerId::from(789),
        };

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: Some(folder_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
            add_folder(folder_id, Owner::mock_current_user(), cloud_model);
        });

        server_api
            .expect_transfer_notebook_owner()
            .returning(move |_, _| Err(anyhow::anyhow!("move failed")));

        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));
        assert_space_for_object(&app, &sync_id.uid(), Space::Personal);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Move the notebook from a personal folder to the team space.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Notebook);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Space(team),
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), true);
        assert_space_for_object(&app, &sync_id.uid(), team);
        // Currently, we can only move to the root of another space.
        assert_root_level_for_object(&mut app, &sync_id.uid(), true);

        // There should be an optimistic model event, but no database write.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: Some(folder_id.into()),
                to_folder: None,
            }]
        );
        assert!(db_events(&update_manager_struct).is_empty());

        // Wait for the move to fail.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // await long enough that all the move object retries are exhausted
        warpui::r#async::Timer::after(Duration::from_secs(10)).await;

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), false);

        // We should roll back the optimistic update and emit a corresponding event.
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));
        assert_space_for_object(&app, &sync_id.uid(), Space::Personal);
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: None,
                to_folder: Some(folder_id.into())
            }]
        );

        // There's no database update to roll back.
        assert!(db_events(&update_manager_struct).is_empty());
    });
}

/// Test successfully moving a Cloud Environment (generic string object) from a user's personal
/// space to a team drive.
#[test]
fn test_move_cloud_environment_personal_to_team_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let environment_id: GenericStringObjectId = server_id.into();
        let sync_id = SyncId::ServerId(environment_id.into());
        let folder_id: FolderId = 456.into();
        let team = Space::Team {
            team_uid: ServerId::from(789),
        };

        let environment = AmbientAgentEnvironment::new(
            "Test Env".to_string(),
            Some("Test description".to_string()),
            vec![],
            "ubuntu:latest".to_string(),
            vec![],
        );

        let mut metadata = crate::cloud_object::CloudObjectMetadata::mock();
        metadata.folder_id = Some(folder_id.into());

        let object = CloudAmbientAgentEnvironment::new(
            sync_id,
            CloudAmbientAgentEnvironmentModel::new(environment),
            metadata,
            crate::cloud_object::CloudObjectPermissions::mock_personal(),
        );

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, object);
            add_folder(folder_id, Owner::mock_current_user(), cloud_model);
        });

        server_api
            .expect_transfer_generic_string_object_owner()
            .times(1)
            .return_once(move |_, _| Ok(true));

        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));
        assert_space_for_object(&app, &sync_id.uid(), Space::Personal);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        let type_and_id = CloudObjectTypeAndId::GenericStringObject {
            object_type: GenericStringObjectFormat::Json(JsonObjectType::CloudEnvironment),
            id: sync_id,
        };

        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Space(team),
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), true);
        assert_space_for_object(&app, &sync_id.uid(), team);
        assert_root_level_for_object(&mut app, &sync_id.uid(), true);

        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: Some(folder_id.into()),
                to_folder: None,
            }]
        );

        // Wait for the move to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), false);
        assert_space_for_object(&app, &sync_id.uid(), team);

        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        let ModelEvent::UpsertGenericStringObject { object } = &events[0] else {
            panic!("Expected upsert of cloud environment, got {:?}", &events[0])
        };
        assert_eq!(object.id(), sync_id);
    });
}

/// Test successfully moving a workflow with workflow enums from a user's personal space to a team drive.
/// This test checks that when we move from personal to team space, we create a new enum in the new space
/// and change the reference stored within the workflow to point to that enum.
#[test]
fn test_move_workflow_with_enums_personal_to_team_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let team = Space::Team {
            team_uid: ServerId::from(789),
        };

        let workflow_server_id: ServerId = 123.into();
        let workflow_id: WorkflowId = workflow_server_id.into();
        let workflow_sync_id = SyncId::ServerId(workflow_id.into());

        let enum_server_id: ServerId = 456.into();
        let enum_id: GenericStringObjectId = enum_server_id.into();
        let enum_sync_id = SyncId::ServerId(enum_id.into());

        let object_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        let (workflow, workflow_enum) = mock_server_workflow_with_enum(
            workflow_id,
            enum_id,
            Owner::mock_current_user(),
            object_metadata,
        );

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(workflow_sync_id, CloudWorkflow::new_from_server(workflow));
            cloud_model.add_object(
                enum_sync_id,
                CloudWorkflowEnum::new_from_server(workflow_enum),
            );
        });

        server_api
            .expect_transfer_workflow_owner()
            .times(1)
            .return_once(move |_, _| Ok(true));

        assert_space_for_object(&app, &workflow_sync_id.uid(), Space::Personal);
        assert_space_for_object(&app, &enum_sync_id.uid(), Space::Personal);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Move the workflow from a personal folder to the team space.
        let type_and_id =
            CloudObjectTypeAndId::from_id_and_type(workflow_sync_id, ObjectType::Workflow);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Space(team),
                    ctx,
                );
            });

        // Wait for the move to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_pending_online_only_change_for_object(&mut app, &workflow_sync_id.uid(), false);
        assert_space_for_object(&app, &workflow_sync_id.uid(), team);

        // Assert cloud events: expect to move the workflow, create a new enum, and update the workflow
        let cloud_events = cloud_events(&update_manager_struct);
        assert_eq!(cloud_events.len(), 4);
        assert!(matches!(
            &cloud_events[0],
            CloudModelEvent::ObjectMoved { .. }
        ));
        assert!(matches!(
            &cloud_events[1],
            CloudModelEvent::ObjectCreated { .. }
        ));
        assert!(matches!(
            &cloud_events[2],
            CloudModelEvent::ObjectForceExpanded { .. }
        ));
        assert!(matches!(
            &cloud_events[3],
            CloudModelEvent::ObjectUpdated { .. }
        ));

        // Assert database events: expect to see the workflow enum creation, them workflow move, then the workflow update
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 3);
        let ModelEvent::UpsertGenericStringObject { object } = &events[0] else {
            panic!("Expected upsert of workflow enum, got {:?}", &events[0])
        };
        let new_enum_id = object.id();
        assert!(
            matches!(&events[1], ModelEvent::UpsertWorkflow { workflow } if workflow.id == workflow_sync_id),
            "Expected upsert of workflow, got {:?}",
            &events[0]
        );
        assert!(
            matches!(&events[2], ModelEvent::UpsertWorkflow { workflow } if workflow.id == workflow_sync_id),
            "Expected upsert of workflow, got {:?}",
            &events[2]
        );

        // Assert that the new enum ID is in the team space, but the old one remains in the personal space
        assert_space_for_object(&app, &new_enum_id.uid(), team);
        assert_space_for_object(&app, &enum_sync_id.uid(), Space::Personal);

        // Assert that the workflow update references the new enum ID
        assert!(
            {
                if let ModelEvent::UpsertWorkflow { workflow } = &events[2] {
                    workflow.model().data.arguments()[0].arg_type
                        == ArgumentType::Enum {
                            enum_id: new_enum_id,
                        }
                } else {
                    false
                }
            },
            "The workflow update should reference the new enum ID"
        );
    });
}

#[test]
fn test_move_workflow_with_enums_personal_to_team_failure() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        let team = Space::Team {
            team_uid: ServerId::from(789),
        };

        let workflow_server_id: ServerId = 123.into();
        let workflow_id: WorkflowId = workflow_server_id.into();
        let workflow_sync_id = SyncId::ServerId(workflow_id.into());

        let enum_server_id: ServerId = 456.into();
        let enum_id: GenericStringObjectId = enum_server_id.into();
        let enum_sync_id = SyncId::ServerId(enum_id.into());

        let object_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        let (workflow, workflow_enum) = mock_server_workflow_with_enum(
            workflow_id,
            enum_id,
            Owner::mock_current_user(),
            object_metadata,
        );

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(workflow_sync_id, CloudWorkflow::new_from_server(workflow));
            cloud_model.add_object(
                enum_sync_id,
                CloudWorkflowEnum::new_from_server(workflow_enum),
            );
        });

        server_api
            .expect_transfer_workflow_owner()
            .returning(move |_, _| Err(anyhow::anyhow!("move failed")));

        assert_space_for_object(&app, &workflow_sync_id.uid(), Space::Personal);
        assert_space_for_object(&app, &enum_sync_id.uid(), Space::Personal);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Move the workflow from a personal folder to the team space.
        let type_and_id =
            CloudObjectTypeAndId::from_id_and_type(workflow_sync_id, ObjectType::Workflow);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Space(team),
                    ctx,
                );
            });

        // Wait for the move to fail.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // await long enough that all the move object retries are exhausted
        warpui::r#async::Timer::after(Duration::from_secs(10)).await;

        // The workflow and enum should remain in the personal space
        assert_pending_online_only_change_for_object(&mut app, &workflow_sync_id.uid(), false);
        assert_space_for_object(&app, &workflow_sync_id.uid(), Space::Personal);
        assert_space_for_object(&app, &enum_sync_id.uid(), Space::Personal);

        // Assert cloud events: expect to move the workflow, create a new enum, and update the workflow,
        // then update it again and move it back to its original space
        let cloud_events = cloud_events(&update_manager_struct);
        assert_eq!(cloud_events.len(), 6);
        assert!(matches!(
            &cloud_events[0],
            CloudModelEvent::ObjectMoved { .. }
        ));
        assert!(matches!(
            &cloud_events[1],
            CloudModelEvent::ObjectCreated { .. }
        ));
        assert!(matches!(
            &cloud_events[2],
            CloudModelEvent::ObjectForceExpanded { .. }
        ));
        assert!(matches!(
            &cloud_events[3],
            CloudModelEvent::ObjectUpdated { .. }
        ));
        assert!(matches!(
            &cloud_events[4],
            CloudModelEvent::ObjectUpdated { .. }
        ));
        assert!(matches!(
            &cloud_events[5],
            CloudModelEvent::ObjectMoved { .. }
        ));

        // Assert database events
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 3);
        assert!(
            matches!(&events[0], ModelEvent::UpsertGenericStringObject { .. }),
            "Expected upsert of GSO, got {:?}",
            &events[0]
        );
        assert!(
            matches!(&events[1], ModelEvent::UpsertWorkflow { workflow } if workflow.id == workflow_sync_id),
            "Expected upsert of workflow, got {:?}",
            &events[0]
        );

        assert!(
            matches!(&events[2], ModelEvent::UpsertWorkflow { workflow } if workflow.id == workflow_sync_id),
            "Expected upsert of workflow, got {:?}",
            &events[2]
        );

        // Assert that the workflow update references the old enum ID
        assert!(
            {
                if let ModelEvent::UpsertWorkflow { workflow } = &events[2] {
                    workflow.model().data.arguments()[0].arg_type
                        == ArgumentType::Enum {
                            enum_id: enum_sync_id,
                        }
                } else {
                    false
                }
            },
            "The workflow update should reference the new enum ID"
        );
    });
}

/// Test moving an object from the root of a space into a folder. This covers the metadata state
/// before, during, and after a move where the object's parent folder changes from `None` to
/// `Some`.
#[test]
fn test_move_object_root_to_folder_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());
        let folder_id: FolderId = 456.into();

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
            add_folder(folder_id, Owner::mock_current_user(), cloud_model);
        });

        server_api
            .expect_move_object()
            .times(1)
            .return_once(move |_, _, _, _| Ok(true));

        assert_root_level_for_object(&mut app, &sync_id.uid(), true);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Move the notebook from the root to a folder.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Notebook);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Folder(folder_id.into()),
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), true);
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));

        // There should be an optimistic model event, but no database write.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: None,
                to_folder: Some(folder_id.into()),
            }]
        );
        assert!(db_events(&update_manager_struct).is_empty());

        // Wait for the move to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), false);
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));

        // After the move succeeds on the server, we update the DB, but don't emit another model
        // event.
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], ModelEvent::UpsertNotebook { notebook } if notebook.id == sync_id),
            "Expected upsert of notebook, got {:?}",
            &events[0]
        );
    });
}

/// Test failing to move an object from the root of a space into a folder. This checks that we
/// optimistically update the model, and then roll back the updates on failure.
#[test]
fn test_move_object_root_to_folder_failure() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());
        let folder_id: FolderId = 456.into();

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
            add_folder(folder_id, Owner::mock_current_user(), cloud_model);
        });

        server_api
            .expect_move_object()
            .returning(move |_, _, _, _| Err(anyhow::anyhow!("move failed")));

        assert_root_level_for_object(&mut app, &sync_id.uid(), true);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Move the notebook from the root to a folder.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Notebook);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Folder(folder_id.into()),
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), true);
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));

        // There should be an optimistic model event, but no database write.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: None,
                to_folder: Some(folder_id.into()),
            }]
        );
        assert!(db_events(&update_manager_struct).is_empty());

        // Wait for the move to fail.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // await long enough that all the move object retries are exhausted
        warpui::r#async::Timer::after(Duration::from_secs(10)).await;

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), false);

        // We should roll back the optimistic update and emit a corresponding event.
        assert_folder_for_object(&app, &sync_id.uid(), None);
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: Some(folder_id.into()),
                to_folder: None
            }]
        );

        // There's no database update to roll back.
        assert!(db_events(&update_manager_struct).is_empty());
    });
}

/// Test moving an object from a folder to the root of its space. This checks metadata before,
/// during, and after a move where the object's parent folder changes from `Some` to `None`.
#[test]
fn test_move_object_folder_to_root_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());
        let folder_id: FolderId = 456.into();

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: Some(folder_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
            add_folder(folder_id, Owner::mock_current_user(), cloud_model);
        });

        server_api
            .expect_move_object()
            .times(1)
            .return_once(move |_, _, _, _| Ok(true));

        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Move the notebook from a folder to the root.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Notebook);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Space(Space::Personal),
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), true);
        assert_root_level_for_object(&mut app, &sync_id.uid(), true);

        // There should be an optimistic model event, but no database write.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: Some(folder_id.into()),
                to_folder: None,
            }]
        );
        assert!(db_events(&update_manager_struct).is_empty());

        // Wait for the move to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), false);
        assert_root_level_for_object(&mut app, &sync_id.uid(), true);

        // After the move succeeds on the server, we update the DB, but don't emit another model
        // event.
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], ModelEvent::UpsertNotebook { notebook } if notebook.id == sync_id),
            "Expected upsert of notebook, got {:?}",
            &events[0]
        );
    });
}

/// Test failing to move an object out of a folder and into the root of its space. This checks that
/// we optimistically clear its parent folder, and then restore it on failure.
#[test]
fn test_move_object_folder_to_root_failure() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());
        let folder_id: FolderId = 456.into();

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: Some(folder_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
            add_folder(folder_id, Owner::mock_current_user(), cloud_model);
        });

        server_api
            .expect_move_object()
            .returning(move |_, _, _, _| Err(anyhow::anyhow!("move failed")));

        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Move the notebook from a folder to the root.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Notebook);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Space(Space::Personal),
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), true);
        assert_root_level_for_object(&mut app, &sync_id.uid(), true);

        // There should be an optimistic model event, but no database write.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: Some(folder_id.into()),
                to_folder: None,
            }]
        );
        assert!(db_events(&update_manager_struct).is_empty());

        // Wait for the move to fail.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // await long enough that all the move object retries are exhausted
        warpui::r#async::Timer::after(Duration::from_secs(10)).await;

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), false);

        // We should roll back the optimistic update and emit a corresponding event.
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: None,
                to_folder: Some(folder_id.into())
            }]
        );

        // There's no database update to roll back.
        assert!(db_events(&update_manager_struct).is_empty());
    });
}

/// Test successfully moving an object from one folder to another, within the same space. This
/// checks that we update the object's metadata and emit events correctly.
#[test]
fn test_move_object_folder_to_folder_success() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());
        let folder_a_id: FolderId = 456.into();
        let folder_b_id: FolderId = 789.into();

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: Some(folder_a_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
            add_folder(folder_a_id, Owner::mock_current_user(), cloud_model);
            add_folder(folder_b_id, Owner::mock_current_user(), cloud_model);
        });

        server_api
            .expect_move_object()
            .times(1)
            .return_once(move |_, _, _, _| Ok(true));

        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_a_id.into()));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Move the notebook from folder A to folder B.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Notebook);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Folder(folder_b_id.into()),
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), true);
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_b_id.into()));

        // There should be an optimistic model event, but no database write.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: Some(folder_a_id.into()),
                to_folder: Some(folder_b_id.into())
            }]
        );
        assert!(db_events(&update_manager_struct).is_empty());

        // Wait for the move to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), false);
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_b_id.into()));

        // After the move succeeds on the server, we update the DB, but don't emit another model
        // event.
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], ModelEvent::UpsertNotebook { notebook } if notebook.id == sync_id),
            "Expected upsert of notebook, got {:?}",
            &events[0]
        );
    });
}

/// Test failing to move an object from one folder to another in the same space. This checks that
/// we optimistically apply the move in-memory and then undo it on failure.
#[test]
fn test_move_object_folder_to_folder_failure() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());
        let folder_a_id: FolderId = 456.into();
        let folder_b_id: FolderId = 789.into();

        let notebook_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: Some(folder_a_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
            add_folder(folder_a_id, Owner::mock_current_user(), cloud_model);
            add_folder(folder_b_id, Owner::mock_current_user(), cloud_model);
        });

        server_api
            .expect_move_object()
            .returning(move |_, _, _, _| Err(anyhow::anyhow!("move failed")));

        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_a_id.into()));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Move the notebook from folder A to folder B.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Notebook);
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.move_object_to_location(
                    type_and_id,
                    CloudObjectLocation::Folder(folder_b_id.into()),
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), true);
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_b_id.into()));

        // There should be an optimistic model event, but no database write.
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: Some(folder_a_id.into()),
                to_folder: Some(folder_b_id.into())
            }]
        );
        assert!(db_events(&update_manager_struct).is_empty());

        // Wait for the move to fail.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // await long enough that all the move object retries are exhausted
        warpui::r#async::Timer::after(Duration::from_secs(10)).await;

        assert_pending_online_only_change_for_object(&mut app, &sync_id.uid(), false);

        // We should roll back the optimistic update and emit a corresponding event.
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_a_id.into()));
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Local,
                from_folder: Some(folder_b_id.into()),
                to_folder: Some(folder_a_id.into())
            }]
        );

        // There's no database update to roll back.
        assert!(db_events(&update_manager_struct).is_empty());
    });
}

/// Tests that the cloud model is updated correctly if we receive an RTC message indicating that an
/// object was trashed by another client.
#[test]
fn test_trash_object_over_rtc() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();
        let workflow_id: WorkflowId = 123.into();
        let sync_id = SyncId::ServerId(workflow_id.into());

        let current_metadata_ts = Utc::now();
        let current_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(
                sync_id,
                CloudWorkflow::new_from_server(mock_server_workflow(
                    workflow_id,
                    Owner::mock_current_user(),
                    current_metadata,
                )),
            );
        });

        assert_trashed_status_for_object(&mut app, &sync_id.uid(), false);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        let new_metadata_ts = current_metadata_ts + chrono::Duration::seconds(1);
        let new_metadata = ServerMetadata {
            uid: workflow_id.into(),
            revision: Revision::now(),
            metadata_last_updated_ts: new_metadata_ts.into(),
            trashed_ts: Some(new_metadata_ts.into()),
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        receive_object_update_from_rtc(
            &mut app,
            &update_manager_struct.update_manager,
            ObjectUpdateMessage::ObjectMetadataChanged {
                metadata: new_metadata,
            },
        );

        // The metadata changes should be applied in-memory and to the database.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow);
        assert_trashed_status_for_object(&mut app, &sync_id.uid(), true);
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectTrashed {
                type_and_id,
                source: UpdateSource::Server
            }]
        );
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            ModelEvent::UpdateObjectMetadata { id, .. } if id == &sync_id.sqlite_uid_hash(ObjectIdType::Workflow)
        ));
    });
}

/// Tests that the cloud model is updated correctly if we receive an RTC message indicating that an
/// object was un-trashed by another client.
#[test]
fn test_untrash_object_over_rtc() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();
        let workflow_id: WorkflowId = 123.into();
        let sync_id = SyncId::ServerId(workflow_id.into());

        let current_metadata_ts = Utc::now();
        let current_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: Some(current_metadata_ts.into()),
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(
                sync_id,
                CloudWorkflow::new_from_server(mock_server_workflow(
                    workflow_id,
                    Owner::mock_current_user(),
                    current_metadata,
                )),
            );
        });

        assert_trashed_status_for_object(&mut app, &sync_id.uid(), true);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        let new_metadata_ts = current_metadata_ts + chrono::Duration::seconds(1);
        let new_metadata = ServerMetadata {
            uid: workflow_id.into(),
            revision: Revision::now(),
            metadata_last_updated_ts: new_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        receive_object_update_from_rtc(
            &mut app,
            &update_manager_struct.update_manager,
            ObjectUpdateMessage::ObjectMetadataChanged {
                metadata: new_metadata,
            },
        );

        // The metadata changes should be applied in-memory and to the database.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow);
        assert_trashed_status_for_object(&mut app, &sync_id.uid(), false);
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectUntrashed {
                type_and_id,
                source: UpdateSource::Server
            }]
        );
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            ModelEvent::UpdateObjectMetadata { id, .. } if id == &sync_id.sqlite_uid_hash(ObjectIdType::Workflow)
        ));
    });
}

/// Tests that the cloud model is correctly updated if we receive an RTC message indicating that an
/// object was moved from one folder to another in the same space by another client.
#[test]
fn test_move_object_from_folder_to_folder_over_rtc() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();
        let workflow_id: WorkflowId = 123.into();
        let sync_id = SyncId::ServerId(workflow_id.into());
        let folder_a_id: FolderId = 456.into();
        let folder_b_id: FolderId = 789.into();

        let current_metadata_ts = Utc::now();
        let current_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: None,
            folder_id: Some(folder_a_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(
                sync_id,
                CloudWorkflow::new_from_server(mock_server_workflow(
                    workflow_id,
                    Owner::mock_current_user(),
                    current_metadata,
                )),
            );
        });

        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_a_id.into()));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        let new_metadata_ts = current_metadata_ts + chrono::Duration::seconds(1);
        let new_metadata = ServerMetadata {
            uid: workflow_id.into(),
            revision: Revision::now(),
            metadata_last_updated_ts: new_metadata_ts.into(),
            trashed_ts: None,
            folder_id: Some(folder_b_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        receive_object_update_from_rtc(
            &mut app,
            &update_manager_struct.update_manager,
            ObjectUpdateMessage::ObjectMetadataChanged {
                metadata: new_metadata,
            },
        );

        // The metadata changes should be applied in-memory and to the database.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow);
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_b_id.into()));
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Server,
                from_folder: Some(folder_a_id.into()),
                to_folder: Some(folder_b_id.into()),
            }]
        );
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            ModelEvent::UpdateObjectMetadata { id, .. } if id == &sync_id.sqlite_uid_hash(ObjectIdType::Workflow)
        ));
    });
}

/// Tests that the cloud model is updated correctly if we receive an RTC message indicating that an
/// object was moved from a folder to the root of its space by another client.
#[test]
fn test_move_object_from_folder_to_root_over_rtc() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();
        let workflow_id: WorkflowId = 123.into();
        let sync_id = SyncId::ServerId(workflow_id.into());
        let folder_id: FolderId = 456.into();

        let current_metadata_ts = Utc::now();
        let current_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: None,
            folder_id: Some(folder_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(
                sync_id,
                CloudWorkflow::new_from_server(mock_server_workflow(
                    workflow_id,
                    Owner::mock_current_user(),
                    current_metadata,
                )),
            );
        });

        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        let new_metadata_ts = current_metadata_ts + chrono::Duration::seconds(1);
        let new_metadata = ServerMetadata {
            uid: workflow_id.into(),
            revision: Revision::now(),
            metadata_last_updated_ts: new_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        receive_object_update_from_rtc(
            &mut app,
            &update_manager_struct.update_manager,
            ObjectUpdateMessage::ObjectMetadataChanged {
                metadata: new_metadata,
            },
        );

        // The metadata changes should be applied in-memory and to the database.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow);
        assert_folder_for_object(&app, &sync_id.uid(), None);
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Server,
                from_folder: Some(folder_id.into()),
                to_folder: None,
            }]
        );
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            ModelEvent::UpdateObjectMetadata { id, .. } if id == &sync_id.sqlite_uid_hash(ObjectIdType::Workflow)
        ));
    });
}

/// Tests that we update the cloud model correctly after receiving an RTC message indicating that
/// an object was moved from the root of its space into a folder by another client.
#[test]
fn test_move_object_from_root_to_folder_over_rtc() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let server_api = mock_server_api();
        let workflow_id: WorkflowId = 123.into();
        let sync_id = SyncId::ServerId(workflow_id.into());
        let folder_id: FolderId = 456.into();

        let current_metadata_ts = Utc::now();
        let current_metadata = ServerMetadata {
            uid: ServerId::default(),
            revision: Revision::now(),
            metadata_last_updated_ts: current_metadata_ts.into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(
                sync_id,
                CloudWorkflow::new_from_server(mock_server_workflow(
                    workflow_id,
                    Owner::mock_current_user(),
                    current_metadata,
                )),
            );
        });

        assert_folder_for_object(&app, &sync_id.uid(), None);

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        let new_metadata_ts = current_metadata_ts + chrono::Duration::seconds(1);
        let new_metadata = ServerMetadata {
            uid: workflow_id.into(),
            revision: Revision::now(),
            metadata_last_updated_ts: new_metadata_ts.into(),
            trashed_ts: None,
            folder_id: Some(folder_id),
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        receive_object_update_from_rtc(
            &mut app,
            &update_manager_struct.update_manager,
            ObjectUpdateMessage::ObjectMetadataChanged {
                metadata: new_metadata,
            },
        );

        // The metadata changes should be applied in-memory and to the database.
        let type_and_id = CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow);
        assert_folder_for_object(&app, &sync_id.uid(), Some(folder_id.into()));
        assert_eq!(
            cloud_events(&update_manager_struct),
            vec![CloudModelEvent::ObjectMoved {
                type_and_id,
                source: UpdateSource::Server,
                from_folder: None,
                to_folder: Some(folder_id.into()),
            }]
        );
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            ModelEvent::UpdateObjectMetadata { id, .. } if id == &sync_id.sqlite_uid_hash(ObjectIdType::Workflow)
        ));
    });
}

#[test]
fn test_permissions_update_grants_access() {
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);

        let mut server_api = mock_server_api();
        let notebook_id: NotebookId = 123.into();
        let sync_id = SyncId::ServerId(notebook_id.into());
        let owner = Owner::Team {
            team_uid: ServerId::from(99),
        };

        let guest_user_id = UserUid::new("abc123");
        let other_guests = vec![UserProfileWithUID {
            firebase_uid: guest_user_id,
            display_name: Some("Warp User".to_string()),
            email: "user@warp.dev".to_string(),
            photo_url: String::new(),
        }];

        let server_permissions = mock_server_permissions(owner);
        let server_notebook = mock_server_notebook(
            notebook_id,
            owner,
            ServerMetadata {
                uid: notebook_id.into(),
                revision: Revision::now(),
                metadata_last_updated_ts: Utc::now().into(),
                trashed_ts: None,
                folder_id: None,
                is_welcome_object: false,
                creator_uid: None,
                last_editor_uid: None,
                current_editor_uid: None,
            },
        );
        let notebook_data = server_notebook.model.data.clone();

        server_api
            .expect_fetch_single_cloud_object()
            .times(1)
            .return_once(move |_| {
                Ok(GetCloudObjectResponse {
                    object: ServerCloudObject::Notebook(server_notebook),
                    action_histories: vec![ObjectActionHistory {
                        uid: notebook_id.to_hash(),
                        hashed_sqlite_id: notebook_id
                            .to_server_id()
                            .sqlite_type_and_uid_hash(ObjectIdType::Notebook),
                        latest_processed_at_timestamp: Utc::now(),
                        actions: vec![],
                    }],
                    descendants: vec![],
                })
            });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        receive_object_update_from_rtc(
            &mut app,
            &update_manager_struct.update_manager,
            ObjectUpdateMessage::ObjectPermissionsChangedV2 {
                object_uid: notebook_id.into(),
                permissions: server_permissions,
                user_profiles: other_guests.clone(),
            },
        );

        // The object isn't in memory yet.
        assert!(CloudModel::handle(&app).read(&app, |cloud_model, _| {
            cloud_model.get_notebook(&sync_id).is_none()
        }),);
        assert!(cloud_events(&update_manager_struct).is_empty());

        // The permissions change should also insert new user profiles.
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], ModelEvent::UpsertUserProfiles { profiles } if profiles == &other_guests)
        );
        UserProfiles::handle(&app).read(&app, |user_profiles, _| {
            assert!(user_profiles.profile_for_uid(guest_user_id).is_some());
        });

        // Wait for the fetch to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        // The object should have been fetched.
        assert_notebook_data(&app, sync_id, &notebook_data);

        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], ModelEvent::UpsertNotebook { .. }));
        assert!(matches!(&events[1], ModelEvent::SyncObjectActions { .. }));

        let events = cloud_events(&update_manager_struct);
        assert_eq!(
            events,
            vec![CloudModelEvent::ObjectCreated {
                type_and_id: CloudObjectTypeAndId::Notebook(sync_id),
            }]
        );
    });
}

#[test]
fn test_permissions_update_existing_object() {
    let _guard = FeatureFlag::SharedWithMe.override_enabled(true);

    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);

        let server_api = mock_server_api();
        let notebook_id: NotebookId = 123.into();
        let sync_id = SyncId::ServerId(notebook_id.into());

        // Model the object already existing in memory.
        let original_server_notebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            ServerMetadata {
                uid: notebook_id.into(),
                revision: Revision::now(),
                metadata_last_updated_ts: Utc::now().into(),
                trashed_ts: None,
                folder_id: None,
                is_welcome_object: false,
                creator_uid: None,
                last_editor_uid: None,
                current_editor_uid: None,
            },
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(
                sync_id,
                CloudNotebook::new_from_server(original_server_notebook),
            );
        });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        assert_space_for_object(&app, &sync_id.uid(), Space::Personal);

        // Receive an RTC update that moves the object.
        receive_object_update_from_rtc(
            &mut app,
            &update_manager_struct.update_manager,
            ObjectUpdateMessage::ObjectPermissionsChangedV2 {
                object_uid: notebook_id.into(),
                permissions: mock_server_permissions(Owner::Team {
                    team_uid: ServerId::from(99),
                }),
                user_profiles: vec![],
            },
        );

        // The object's space should change.
        assert_space_for_object(&app, &sync_id.uid(), Space::Shared);

        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], ModelEvent::UpsertNotebook { .. }));

        // We don't currently emit CloudModel events for permission changes - should we?
    });
}

#[test]
fn test_add_guest_success() {
    let _guard = FeatureFlag::SharedWithMe.override_enabled(true);
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let uid = server_id.uid();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());

        let notebook_metadata = ServerMetadata {
            uid: server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        let updated_permissions_ts = Utc::now() + chrono::Duration::seconds(5);
        let updated_permissions = ServerPermissions {
            space: Owner::mock_current_user(),
            guests: vec![ServerObjectGuest {
                subject: ServerGuestSubject::User {
                    firebase_uid: "guest".to_string(),
                },
                access_level: AccessLevel::Editor,
                source: None,
            }],
            anyone_link_sharing: None,
            permissions_last_updated_ts: updated_permissions_ts.into(),
        };

        server_api
            .expect_add_object_guests()
            .times(1)
            .return_once(move |_, _, _| {
                Ok(ObjectPermissionsUpdateData {
                    permissions: updated_permissions,
                    profiles: vec![UserProfileWithUID {
                        firebase_uid: UserUid::new("guest"),
                        display_name: Some("Guest User".to_string()),
                        email: "guest@warp.dev".to_string(),
                        photo_url: "http://example.com".to_string(),
                    }],
                })
            });

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Add the user as a guest.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.add_object_guests(
                    server_id,
                    vec!["guest@warp.dev".to_string()],
                    AccessLevel::Editor,
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &uid, true);

        // There should be no optimistic update.
        assert_eq!(cloud_events(&update_manager_struct), vec![]);
        assert!(db_events(&update_manager_struct).is_empty());

        // Wait for the permissions change to complete.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;

        assert_pending_online_only_change_for_object(&mut app, &uid, false);
        CloudModel::handle(&app).read(&app, |cloud_model, _| {
            let object = cloud_model.get_by_uid(&uid).expect("Object should exist");
            assert_eq!(
                object.permissions().guests,
                vec![CloudObjectGuest {
                    subject: Subject::User(UserKind::Account(UserUid::new("guest"))),
                    access_level: SharingAccessLevel::Edit,
                    source: None,
                }]
            )
        });

        // After the update succeeds on the server, we update the DB.
        let events = db_events(&update_manager_struct);
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0], ModelEvent::UpsertNotebook { notebook } if notebook.id == sync_id),
            "Expected upsert of notebook, got {:?}",
            &events[0]
        );
        assert!(matches!(&events[1], ModelEvent::UpsertUserProfiles { .. }));
    });
}

#[test]
fn test_add_guest_failure() {
    let _guard = FeatureFlag::SharedWithMe.override_enabled(true);
    App::test(ASSETS, |mut app| async move {
        initialize_app(&mut app);
        let mut server_api = mock_server_api();

        let server_id: ServerId = 123.into();
        let uid = server_id.uid();
        let notebook_id: NotebookId = server_id.into();
        let sync_id = SyncId::ServerId(notebook_id.into());

        let notebook_metadata = ServerMetadata {
            uid: server_id,
            revision: Revision::now(),
            metadata_last_updated_ts: Utc::now().into(),
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        };
        let notebook: ServerNotebook = mock_server_notebook(
            notebook_id,
            Owner::mock_current_user(),
            notebook_metadata.clone(),
        );
        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(sync_id, CloudNotebook::new_from_server(notebook));
        });

        server_api
            .expect_add_object_guests()
            .returning(move |_, _, _| Err(anyhow::anyhow!("adding guest failed")));

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        // Add the user as a guest.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                update_manager.add_object_guests(
                    server_id,
                    vec!["guest@warp.dev".to_string()],
                    AccessLevel::Editor,
                    ctx,
                );
            });

        assert_pending_online_only_change_for_object(&mut app, &uid, true);

        // There should be no optimistic update.
        assert_eq!(cloud_events(&update_manager_struct), vec![]);
        assert!(db_events(&update_manager_struct).is_empty());

        // Wait for the permissions change and all retries to fail.
        update_manager_struct
            .update_manager
            .update(&mut app, |update_manager, ctx| {
                ctx.await_spawned_future(update_manager.spawned_futures[0])
            })
            .await;
        warpui::r#async::Timer::after(Duration::from_secs(10)).await;

        assert_pending_online_only_change_for_object(&mut app, &uid, false);

        // There should no changes to roll back.
        CloudModel::handle(&app).read(&app, |cloud_model, _| {
            let object = cloud_model.get_by_uid(&uid).expect("Object should exist");
            assert_eq!(object.permissions().guests, vec![])
        });
        assert_eq!(cloud_events(&update_manager_struct), vec![]);
        assert!(db_events(&update_manager_struct).is_empty());
    });
}

/// Agenterm's Drive is local: a created workflow must land in the in-memory model and produce a
/// SQLite upsert, and an update must be visible without any server round trip.
#[test]
fn test_local_workflow_persists_without_server_round_trip() {
    App::test(ASSETS, |mut app| async move {
        let client_id = ClientId::new();
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        // No cloud call may happen: the mock panics on unexpected invocations.
        server_api.expect_create_workflow().never();
        server_api.expect_update_workflow().never();

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));
        let update_manager = update_manager_struct.update_manager.clone();

        create_workflow(client_id, &mut app, &update_manager);
        let sync_id = SyncId::ClientId(client_id);

        assert_eq!(
            get_workflow(&app, sync_id).model().data.command(),
            Some("echo client"),
            "the workflow should be readable from the in-memory model"
        );
        assert!(
            !db_events(&update_manager_struct).is_empty(),
            "creating a workflow must write it to SQLite"
        );

        update_manager.update(&mut app, |update_manager, ctx| {
            update_manager.update_workflow(
                Workflow::new("client_workflow".to_string(), "echo updated".to_string()),
                sync_id,
                None,
                ctx,
            );
        });

        assert_eq!(
            get_workflow(&app, sync_id).model().data.command(),
            Some("echo updated"),
            "the update should be visible without a server round trip"
        );
    });
}

#[test]
fn test_local_workflow_trash_restore_and_delete_are_persisted() {
    App::test(ASSETS, |mut app| async move {
        let client_id = ClientId::new();
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        server_api.expect_delete_object().never();

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));
        let update_manager = update_manager_struct.update_manager.clone();
        create_workflow(client_id, &mut app, &update_manager);
        db_events(&update_manager_struct);

        let sync_id = SyncId::ClientId(client_id);
        update_manager.update(&mut app, |update_manager, ctx| {
            update_manager.trash_object(
                CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow),
                ctx,
            );
        });

        app.read(|ctx| {
            let workflow = CloudModel::as_ref(ctx)
                .get_by_uid(&sync_id.uid())
                .expect("trashed workflow remains in the model");
            assert!(workflow.metadata().trashed_ts.is_some());
        });
        assert!(!db_events(&update_manager_struct).is_empty());

        update_manager.update(&mut app, |update_manager, ctx| {
            update_manager.untrash_object(
                CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow),
                ctx,
            );
        });
        app.read(|ctx| {
            let workflow = CloudModel::as_ref(ctx)
                .get_by_uid(&sync_id.uid())
                .expect("restored workflow remains in the model");
            assert!(workflow.metadata().trashed_ts.is_none());
        });
        assert!(!db_events(&update_manager_struct).is_empty());

        update_manager.update(&mut app, |update_manager, ctx| {
            update_manager.delete_object_by_user(
                CloudObjectTypeAndId::from_id_and_type(sync_id, ObjectType::Workflow),
                ctx,
            );
        });
        app.read(|ctx| {
            assert!(CloudModel::as_ref(ctx).get_by_uid(&sync_id.uid()).is_none());
        });
        assert!(db_events(&update_manager_struct).iter().any(|event| {
            matches!(event, ModelEvent::DeleteObjects { ids } if ids.iter().any(|(id, _)| *id == sync_id))
        }));
    });
}

/// Regression: a workflow created without an account must be visible in the personal space.
///
/// The owner id of the local personal drive has to stay stable, because it is persisted on every
/// object. If it changed (or depended on a logged-in user), saved workflows would disappear from
/// the personal space after a restart.
#[test]
fn test_locally_created_workflow_appears_in_personal_space() {
    App::test(ASSETS, |mut app| async move {
        let client_id = ClientId::new();
        initialize_app(&mut app);
        let mut server_api = mock_server_api();
        server_api.expect_create_workflow().never();

        let update_manager_struct = create_update_manager_struct(&mut app, Arc::new(server_api));

        app.update(|ctx| {
            let auth_state = AuthStateProvider::as_ref(ctx).get();
            auth_state.set_user(None);
            auth_state.set_credentials(None);
        });

        let owner = app.read(|ctx| {
            UserWorkspaces::as_ref(ctx)
                .personal_drive(ctx)
                .expect("the local personal drive must exist without an account")
        });

        create_workflow_internal(
            &mut app,
            &update_manager_struct.update_manager,
            client_id,
            "local_workflow".to_string(),
            "echo local".to_string(),
            owner,
            None,
        );

        app.read(|ctx| {
            let names: Vec<String> = CloudModel::as_ref(ctx)
                .active_non_welcome_workflows_in_space(Space::Personal, ctx)
                .map(|workflow| workflow.model().data.name().to_string())
                .collect();
            assert!(
                names.contains(&"local_workflow".to_string()),
                "the created workflow must show up in the personal space, got {names:?}"
            );
        });
    });
}
