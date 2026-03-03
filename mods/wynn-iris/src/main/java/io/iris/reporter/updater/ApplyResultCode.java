package io.iris.reporter.updater;

public enum ApplyResultCode {
    APPLIED("applied"),
    STAGED("staged"),
    FAILED("failed"),
    UPDATE_REPO_MISSING("update_repo_missing"),
    UPDATE_REPO_INVALID("update_repo_invalid"),
    CURRENT_VERSION_INVALID("current_version_invalid"),
    CURRENT_JAR_PATH_UNAVAILABLE("current_jar_path_unavailable"),
    CURRENT_JAR_MISSING("current_jar_missing"),
    CURRENT_JAR_PARENT_MISSING("current_jar_parent_missing"),
    UPDATE_PAYLOAD_EMPTY("update_payload_empty"),
    UPDATE_ASSET_URL_MISSING("update_asset_url_missing"),
    UPDATE_ASSET_URL_INVALID("update_asset_url_invalid"),
    UPDATE_ASSET_URL_INSECURE("update_asset_url_insecure"),
    UPDATE_ASSET_URL_MISSING_HOST("update_asset_url_missing_host"),
    UPDATE_ASSET_HOST_NOT_ALLOWED("update_asset_host_not_allowed"),
    UPDATE_DOWNLOAD_IO("update_download_io"),
    UPDATE_DOWNLOAD_INTERRUPTED("update_download_interrupted"),
    UPDATE_DOWNLOAD_REDIRECT_MISSING_LOCATION("update_download_redirect_missing_location"),
    UPDATE_DOWNLOAD_REDIRECT_INVALID("update_download_redirect_invalid"),
    UPDATE_DOWNLOAD_TOO_MANY_REDIRECTS("update_download_too_many_redirects"),
    UPDATE_DOWNLOAD_EMPTY("update_download_empty"),
    UPDATE_HASH_MISSING("update_hash_missing"),
    UPDATE_HASH_MISMATCH("update_hash_mismatch"),
    UPDATE_INSTALL_FAILED("update_install_failed"),
    UPDATE_HELPER_PREPARE_FAILED("update_helper_prepare_failed"),
    UPDATE_HELPER_START_FAILED("update_helper_start_failed"),
    UPDATE_MANIFEST_MISSING("update_manifest_missing"),
    UPDATE_MANIFEST_SIGNATURE_MISSING("update_manifest_signature_missing"),
    UPDATE_MANIFEST_DOWNLOAD_FAILED("update_manifest_download_failed"),
    UPDATE_MANIFEST_INVALID("update_manifest_invalid"),
    UPDATE_MANIFEST_SIGNATURE_INVALID("update_manifest_signature_invalid"),
    UPDATE_MANIFEST_ASSET_MISSING("update_manifest_asset_missing"),
    UPDATE_MANIFEST_ASSET_HASH_MISSING("update_manifest_asset_hash_missing"),
    UPDATE_MANIFEST_ASSET_HASH_INVALID("update_manifest_asset_hash_invalid"),
    UPDATE_MANIFEST_ASSET_SIZE_INVALID("update_manifest_asset_size_invalid"),
    UPDATE_MANIFEST_ASSET_VERSION_MISMATCH("update_manifest_asset_version_mismatch"),
    UPDATE_MANIFEST_ASSET_MINECRAFT_MISMATCH("update_manifest_asset_minecraft_mismatch"),
    UPDATE_CHECK_FAILED("update_check_failed"),
    UPDATE_APPLY_FAILED("update_apply_failed"),
    UPDATE_JOB_STATUS_INVALID("update_job_status_invalid"),
    UPDATE_JOB_STATUS_MISSING("update_job_status_missing");

    private final String code;

    ApplyResultCode(String code) {
        this.code = code;
    }

    public String code() {
        return code;
    }
}
