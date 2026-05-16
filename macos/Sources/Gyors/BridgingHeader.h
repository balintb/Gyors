#ifndef GYORS_BRIDGING_HEADER_H
#define GYORS_BRIDGING_HEADER_H

#include <stdint.h>
#include <stdbool.h>

void gyors_init(void);
char* gyors_query(const char* pattern);
char* gyors_activate(const char* id, const char* action);
void gyors_record_clipboard(const char* content);
void gyors_clear_clipboard_history(void);
void gyors_free_string(char* s);
uint64_t gyors_app_count(void);
uint64_t gyors_clipboard_count(void);
char* gyors_notes_folder(void);
char* gyors_diagnostics(void);
char* gyors_plugin_install_preview(const char* spec_json);
char* gyors_plugin_install_commit(const char* spec_json);
char* gyors_config_fields_json(void);
void gyors_record_query(const char* pattern);
char* gyors_recent_queries(uint32_t limit);
char* gyors_last_query(void);
char* gyors_snippets_json(void);
bool  gyors_snippets_global_enabled(void);

// Cloud sync - see crates/gyors-sync. Each returns a JSON string the
// caller frees with gyors_free_string. Status is read-only; the rest
// run on the bridge's tokio runtime so they may take a few hundred ms
// (Argon2id is the dominant cost on signup/signin).
//
// Gated on GYORS_CLOUD so a no-cloud build (build-app.sh
// WITH_CLOUD=0) doesn't declare symbols the staticlib no longer
// exports. The Swift call sites are guarded by `#if CLOUD` so they
// disappear at the same time.
#ifdef GYORS_CLOUD
char* gyors_sync_status(void);
char* gyors_sync_signup(const char* email, const char* password);
char* gyors_sync_signin(const char* email, const char* password, const char* kdf_salt);
char* gyors_sync_signout(void);
char* gyors_sync_tick(void);
#endif

#endif
