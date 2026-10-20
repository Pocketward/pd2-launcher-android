@file:OptIn(ExperimentalSerializationApi::class)

package com.pd2launcher.app.control

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonClassDiscriminator

// The Control channel's messages, field for field and in the same order as the
// engine's serde types for the channel (the source of truth). Every Rust
// `Option` is a nullable field without a default, so it is always written (as
// null when absent), as serde writes it. Integers: u32 and u64 are Long, u8 Int.

/** The protocol version this app speaks (the engine's `PROTOCOL`). */
const val PROTOCOL: Int = 4

/** The JSON settings both directions use: internally tagged by `type`, strict. */
val ControlJson: Json = Json { classDiscriminator = "type" }

/** What the app sends. */
@Serializable
sealed class AppMessage {
    /** The first message, and only the first. */
    @Serializable
    @SerialName("hello")
    data class Hello(
        val protocol: Int,
        @SerialName("app_version") val appVersion: String,
        @SerialName("host_facts") val hostFacts: HostFacts,
    ) : AppMessage()

    /** One player action; `seq` is the app's own counter. */
    @Serializable
    @SerialName("command")
    data class CommandMessage(val seq: Long, val intent: Command) : AppMessage()
}

/** What the engine sends. */
@Serializable
sealed class EngineMessage {
    @Serializable
    @SerialName("welcome")
    data class Welcome(
        val protocol: Int,
        @SerialName("launcher_version") val launcherVersion: String,
    ) : EngineMessage()

    @Serializable
    @SerialName("refused")
    data class Refused(val supported: List<Long>) : EngineMessage()

    /** The whole state the app shows; `seq` counts the snapshots sent, from 1. */
    @Serializable
    @SerialName("snapshot")
    data class SnapshotMessage(val seq: Long, val snapshot: Snapshot) : EngineMessage()

    /** The command `seq` was not taken (0: not known, or the engine failed). */
    @Serializable
    @SerialName("command_error")
    data class CommandError(val seq: Long, val reason: String) : EngineMessage()
}

/** The Android host, as the app measured it before starting the engine. */
@Serializable
data class HostFacts(
    @SerialName("android_release") val androidRelease: String,
    val sdk: Long,
    @SerialName("device_model") val deviceModel: String,
    @SerialName("gpu_name") val gpuName: String,
    @SerialName("chip_id") val chipId: Long,
    @SerialName("kernel_release") val kernelRelease: String,
    @SerialName("page_size") val pageSize: Long,
    @SerialName("data_dir") val dataDir: String,
) {
    /** Holds [collect] (HostFactsCollect.kt), which needs an Android context. */
    companion object
}

/** A player action. */
@Serializable
sealed class Command {
    @Serializable @SerialName("navigate")
    data class Navigate(val destination: Destination) : Command()

    @Serializable @SerialName("select_installer_media")
    data class SelectInstallerMedia(val path: String) : Command()

    @Serializable @SerialName("use_blizzard_download")
    data object UseBlizzardDownload : Command()

    @Serializable @SerialName("edit_step")
    data class EditStep(val step: Step) : Command()

    @Serializable @SerialName("close_editor")
    data object CloseEditor : Command()

    @Serializable @SerialName("stop_and_change")
    data class StopAndChange(val step: Step) : Command()

    @Serializable @SerialName("dismiss_stop_result")
    data object DismissStopResult : Command()

    @Serializable @SerialName("dismiss_result")
    data object DismissResult : Command()

    @Serializable @SerialName("check_again")
    data object CheckAgain : Command()

    @Serializable @SerialName("save_download_source")
    data class SaveDownloadSource(val artifact: Artifact, val url: String) : Command()

    @Serializable @SerialName("reset_download_source")
    data class ResetDownloadSource(val artifact: Artifact) : Command()

    /**
     * Both CD keys, sent after the player confirmed Blizzard's terms in the app's dialog. This
     * class exists for the decoder tests and fixtures only: the app sends keys with
     * [ControlChannel.sendAuthorize], which never makes them a String. `toString` hides them.
     */
    @Serializable @SerialName("authorize")
    class Authorize(
        @SerialName("d2_key") val d2Key: String,
        @SerialName("lod_key") val lodKey: String,
    ) : Command() {
        override fun toString(): String = "Authorize([REDACTED], [REDACTED])"

        override fun equals(other: Any?): Boolean = other is Authorize && d2Key == other.d2Key && lodKey == other.lodKey

        override fun hashCode(): Int = 0
    }

    @Serializable @SerialName("continue_installation")
    data object ContinueInstallation : Command()

    @Serializable @SerialName("download_all")
    data object DownloadAll : Command()

    @Serializable @SerialName("import_existing_install")
    data class ImportExistingInstall(val mode: ImportMode) : Command()

    @Serializable @SerialName("install_project_diablo")
    data object InstallProjectDiablo : Command()

    @Serializable @SerialName("select_existing_install")
    data class SelectExistingInstall(val path: String) : Command()

    @Serializable @SerialName("use_default_installer_source")
    data object UseDefaultInstallerSource : Command()

    @Serializable @SerialName("request_safe_stop")
    data object RequestSafeStop : Command()

    @Serializable @SerialName("apply_update")
    data object ApplyUpdate : Command()

    /**
     * Play, with the X screen's size in pixels when known (the game resolution), the graphics
     * driver and the D2GL graphics profile (Automatic resolved; null: the game's graphics stay).
     */
    @Serializable @SerialName("launch_game")
    data class LaunchGame(
        val display: DisplaySize?,
        @SerialName("graphics_driver") val graphicsDriver: GraphicsDriver,
        @SerialName("graphics_profile") val graphicsProfile: GraphicsProfile?,
    ) : Command()

    /** The official PD2 launcher, in the game environment; the press of its button is the confirmation. */
    @Serializable @SerialName("open_official_launcher")
    data object OpenOfficialLauncher : Command()

    @Serializable @SerialName("start_over")
    data object StartOver : Command()

    @Serializable @SerialName("retry_location")
    data object RetryLocation : Command()

    @Serializable @SerialName("change_settings")
    data class ChangeSettings(val change: SettingChange) : Command()

    @Serializable @SerialName("export_diagnostics")
    data object ExportDiagnostics : Command()

    @Serializable @SerialName("reveal_report")
    data object RevealReport : Command()

    @Serializable @SerialName("dismiss_report")
    data object DismissReport : Command()

    @Serializable @SerialName("request_full_snapshot")
    data object RequestFullSnapshot : Command()
}

@Serializable
enum class Destination {
    @SerialName("operations") OPERATIONS,
    @SerialName("settings") SETTINGS,
    @SerialName("diagnostics") DIAGNOSTICS,
}

/** One of the Launcher's six setup steps. */
@Serializable
enum class Step {
    @SerialName("device") DEVICE,
    @SerialName("setup") SETUP,
    @SerialName("download") DOWNLOAD,
    @SerialName("install_d2") INSTALL_D2,
    @SerialName("install_pd2") INSTALL_PD2,
    @SerialName("play") PLAY,
}

@Serializable
enum class ImportMode {
    @SerialName("copy") COPY,
    @SerialName("in_place") IN_PLACE,
}

@Serializable
enum class Artifact {
    @SerialName("wine") WINE,
    @SerialName("gecko") GECKO,
    @SerialName("wine_mono") WINE_MONO,
    @SerialName("dotnet_desktop") DOTNET_DESKTOP,
    @SerialName("d2_installer") D2_INSTALLER,
    @SerialName("lod_installer") LOD_INSTALLER,
    @SerialName("pd2_installer") PD2_INSTALLER,
    @SerialName("pd2_client_base") PD2_CLIENT_BASE,
    @SerialName("fex") FEX,
    @SerialName("fex_root_fs") FEX_ROOT_FS,
}

@Serializable
data class DisplaySize(val width: Long, val height: Long)

/**
 * The graphics driver a Play uses: the App userland's Turnip as it is, or with its Compatibility
 * values (`PD2_APP_WINE_COMPATIBILITY_*` in userland.env).
 */
@Serializable
enum class GraphicsDriver {
    @SerialName("standard") STANDARD,
    @SerialName("compatibility") COMPATIBILITY,
}

/**
 * D2GL's graphics profile a Play sends ([com.pd2launcher.app.GraphicsProfiles]): the engine writes
 * its values into the game's `d2gl.json` when it is not the profile last applied there.
 */
@Serializable
enum class GraphicsProfile {
    /** Project Diablo 2's own D2GL settings. */
    @SerialName("default") DEFAULT,

    /** D2GL's advanced settings tuned down for older devices: a simpler shader, no sharpening, no bloom. */
    @SerialName("performance") PERFORMANCE,
}

@Serializable
@JsonClassDiscriminator("setting")
sealed class SettingChange {
    @Serializable @SerialName("renderer")
    data class Renderer(val renderer: com.pd2launcher.app.control.Renderer) : SettingChange()

    @Serializable @SerialName("direct_draw_renderer")
    data class DirectDrawRenderer(val renderer: com.pd2launcher.app.control.DirectDrawRenderer) : SettingChange()

    @Serializable @SerialName("skip_to_bnet")
    data class SkipToBnet(val enabled: Boolean) : SettingChange()

    @Serializable @SerialName("sound_in_background")
    data class SoundInBackground(val enabled: Boolean) : SettingChange()
}

@Serializable
enum class Renderer {
    @SerialName("glide") GLIDE,
    @SerialName("direct_draw") DIRECT_DRAW,
}

@Serializable
enum class DirectDrawRenderer {
    @SerialName("pd2") PD2,
    @SerialName("open_gl") OPEN_GL,
    @SerialName("direct3d9") DIRECT3D9,
    @SerialName("software") SOFTWARE,
}

/** Everything the app's screens show, as one value. */
@Serializable
data class Snapshot(
    val host: Host,
    val missing: List<Finding>,
    val installation: Installation,
    @SerialName("interrupted_stage") val interruptedStage: Stage?,
    val update: Update,
    val step: Step,
    @SerialName("running_step") val runningStep: Step?,
    val editing: Step?,
    val authorized: Boolean,
    val installer: Installer,
    @SerialName("installer_rejection") val installerRejection: String?,
    val operation: Operation?,
    @SerialName("active_job") val activeJob: Job?,
    val result: ResultCard?,
    @SerialName("stop_result") val stopResult: StopResult?,
    val game: Game,
    @SerialName("game_notice") val gameNotice: GameNotice?,
    @SerialName("launch_preflight_passed") val launchPreflightPassed: Boolean,
    @SerialName("unresolved_journal") val unresolvedJournal: Boolean,
    @SerialName("game_software_rendering") val gameSoftwareRendering: Boolean,
    /** The last audit found the game environment set up: Install's first task is done. */
    @SerialName("environment_ready") val environmentReady: Boolean,
    val downloads: Downloads,
    val location: Location?,
    @SerialName("location_error") val locationError: String?,
    @SerialName("game_folder") val gameFolder: GameFolder?,
    val settings: Settings,
    @SerialName("download_sources") val downloadSources: List<DownloadSource>,
    val report: Report?,
)

@Serializable
enum class Host {
    @SerialName("checking") CHECKING,
    @SerialName("supported") SUPPORTED,
    @SerialName("unsupported") UNSUPPORTED,
}

/**
 * A host finding by its wire name. One this app does not know yet (a newer engine's) is
 * [UNKNOWN] ([FindingSerializer]), so the snapshot still decodes and the finding still shows.
 */
@Serializable(with = FindingSerializer::class)
enum class Finding(val wire: String) {
    DISPLAY_SESSION("display_session"),
    FEX("fex"),
    FEX_ROOT_FS("fex_root_fs"),
    PAGE_SIZE("page_size"),
    WINE_CANNOT_RUN("wine_cannot_run"),
    IA32_EXECUTION("ia32_execution"),
    NO_EXEC("no_exec"),
    I386_LOADER("i386_loader"),
    LIBRARIES("libraries"),
    WINE_LIBRARY_X11("wine_library_x11"),
    WINE_LIBRARY_XEXT("wine_library_xext"),
    WINE_LIBRARY_FREE_TYPE("wine_library_free_type"),
    WINE_LIBRARY_FONTCONFIG("wine_library_fontconfig"),
    WINE_LIBRARY_OPEN_GL("wine_library_open_gl"),
    LIBUNWIND("libunwind"),
    X_WAYLAND("xwayland"),
    VULKAN_LOADER("vulkan_loader"),
    VULKAN_DRIVER("vulkan_driver"),
    HARDWARE_GRAPHICS("hardware_graphics"),
    LAUNCHER_BUILD("launcher_build"),
    ARCHITECTURE("architecture"),
    HOST_INFORMATION("host_information"),
    UNCERTIFIED_BUILD("uncertified_build"),
    /** Not one of the above: never sent by this engine version. */
    UNKNOWN("unknown"),
}

object FindingSerializer : KSerializer<Finding> {
    override val descriptor = PrimitiveSerialDescriptor("com.pd2launcher.app.control.Finding", PrimitiveKind.STRING)

    override fun serialize(encoder: Encoder, value: Finding) = encoder.encodeString(value.wire)

    override fun deserialize(decoder: Decoder): Finding {
        val wire = decoder.decodeString()
        return Finding.entries.firstOrNull { it.wire == wire } ?: Finding.UNKNOWN
    }
}

@Serializable
enum class Installation {
    @SerialName("needs_base_game") NEEDS_BASE_GAME,
    @SerialName("needs_expansion") NEEDS_EXPANSION,
    @SerialName("needs_client") NEEDS_CLIENT,
    /**
     * The game is installed and a part of the game environment is missing. The desktop sets it up
     * in place; for an installed game the app offers only Repair's Start over ([com.pd2launcher.app.Onboarding.installed]).
     */
    @SerialName("needs_runtime") NEEDS_RUNTIME,
    @SerialName("repair_required") REPAIR_REQUIRED,
    @SerialName("indeterminate") INDETERMINATE,
    @SerialName("stage_interrupted") STAGE_INTERRUPTED,
    @SerialName("ready") READY,
}

@Serializable
enum class Stage {
    @SerialName("prefix") PREFIX,
    @SerialName("base_game") BASE_GAME,
    @SerialName("expansion") EXPANSION,
}

@Serializable
enum class Update {
    @SerialName("unknown") UNKNOWN,
    @SerialName("checking") CHECKING,
    @SerialName("up_to_date") UP_TO_DATE,
    @SerialName("available") AVAILABLE,
    @SerialName("check_failed") CHECK_FAILED,
}

/** How Diablo II is to be installed. */
@Serializable
@JsonClassDiscriminator("kind")
sealed class Installer {
    @Serializable @SerialName("not_selected")
    data object NotSelected : Installer()

    @Serializable @SerialName("folder")
    data class Folder(val path: String) : Installer()

    @Serializable @SerialName("blizzard_download")
    data object BlizzardDownload : Installer()

    @Serializable @SerialName("invalid")
    data class Invalid(val reason: String) : Installer()

    @Serializable @SerialName("existing")
    data class Existing(
        val path: String,
        val problem: ExistingProblem?,
        @SerialName("copy_bytes") val copyBytes: Long,
        @SerialName("in_place_possible") val inPlacePossible: Boolean,
    ) : Installer()
}

@Serializable
@JsonClassDiscriminator("code")
sealed class ExistingProblem {
    @Serializable @SerialName("not_found") data object NotFound : ExistingProblem()
    @Serializable @SerialName("unreadable") data object Unreadable : ExistingProblem()
    @Serializable @SerialName("not_diablo") data object NotDiablo : ExistingProblem()

    @Serializable @SerialName("unsupported_version")
    data class UnsupportedVersion(val version: String?) : ExistingProblem()

    @Serializable @SerialName("modified_game") data object ModifiedGame : ExistingProblem()
    @Serializable @SerialName("archive_mismatch") data object ArchiveMismatch : ExistingProblem()
    @Serializable @SerialName("no_expansion") data object NoExpansion : ExistingProblem()

    @Serializable @SerialName("missing_file")
    data class MissingFile(val file: String) : ExistingProblem()

    @Serializable @SerialName("links") data object Links : ExistingProblem()
    @Serializable @SerialName("inside_launcher") data object InsideLauncher : ExistingProblem()
    @Serializable @SerialName("unsupported_path") data object UnsupportedPath : ExistingProblem()
}

@Serializable
data class Operation(
    val phase: Phase,
    val status: OperationStatus,
    val title: String,
    val detail: String,
    val progress: Progress?,
    val mutating: Boolean,
    val failure: String?,
    val job: Job?,
    /** While the game environment is set up: the part running, once the first has started. */
    val environment: EnvironmentSetup?,
)

/** One setup of the game environment: the parts it runs, in order, and the one running. */
@Serializable
data class EnvironmentSetup(val part: EnvironmentPart, val parts: List<EnvironmentPart>)

@Serializable
enum class EnvironmentPart {
    /** Wine unpacked and Windows set up. */
    @SerialName("windows") WINDOWS,
    /** Wine Gecko. */
    @SerialName("web_components") WEB_COMPONENTS,
    /** Wine Mono. */
    @SerialName("dotnet") DOTNET,
    /** The .NET Desktop Runtime the official PD2 launcher needs. */
    @SerialName("launcher_support") LAUNCHER_SUPPORT,
}

@Serializable
enum class Phase {
    @SerialName("host_audit") HOST_AUDIT,
    @SerialName("runtime_acquisition") RUNTIME_ACQUISITION,
    @SerialName("prefix_initialization") PREFIX_INITIALIZATION,
    @SerialName("base_game_install") BASE_GAME_INSTALL,
    @SerialName("expansion_install") EXPANSION_INSTALL,
    @SerialName("base_game_download") BASE_GAME_DOWNLOAD,
    @SerialName("expansion_download") EXPANSION_DOWNLOAD,
    @SerialName("import_game") IMPORT_GAME,
    @SerialName("client_download") CLIENT_DOWNLOAD,
    @SerialName("verification") VERIFICATION,
    @SerialName("promotion") PROMOTION,
    @SerialName("repair") REPAIR,
    @SerialName("game_execution") GAME_EXECUTION,
    @SerialName("official_launcher") OFFICIAL_LAUNCHER,
    @SerialName("reset") RESET,
    @SerialName("relocation") RELOCATION,
}

@Serializable
enum class OperationStatus {
    @SerialName("queued") QUEUED,
    @SerialName("planning") PLANNING,
    @SerialName("running") RUNNING,
    @SerialName("safe_stopping") SAFE_STOPPING,
    @SerialName("verifying") VERIFYING,
    @SerialName("committing") COMMITTING,
    @SerialName("succeeded") SUCCEEDED,
    @SerialName("failed") FAILED,
    @SerialName("indeterminate") INDETERMINATE,
    @SerialName("cancelled") CANCELLED,
}

/** How far the operation is; each part only when known. */
@Serializable
data class Progress(
    val percent: Int?,
    val completed: Long?,
    val total: Long?,
    val unit: String?,
    val summary: String?,
)

@Serializable
enum class Job {
    @SerialName("download") DOWNLOAD,
    @SerialName("install_base_game") INSTALL_BASE_GAME,
    @SerialName("install_project_diablo") INSTALL_PROJECT_DIABLO,
    @SerialName("update") UPDATE,
    @SerialName("repair") REPAIR,
    @SerialName("official_launcher") OFFICIAL_LAUNCHER,
    @SerialName("reset") RESET,
    @SerialName("relocation") RELOCATION,
}

@Serializable
data class ResultCard(val title: String, val message: String, val next: ResultNext?)

@Serializable
enum class ResultNext {
    @SerialName("try_again") TRY_AGAIN,
    @SerialName("open_diagnostics") OPEN_DIAGNOSTICS,
    @SerialName("start_over") START_OVER,
}

@Serializable
data class StopResult(
    val installation: Installation,
    @SerialName("interrupted_stage") val interruptedStage: Stage?,
    @SerialName("needs_recovery") val needsRecovery: Boolean,
)

@Serializable
enum class Game {
    @SerialName("idle") IDLE,
    @SerialName("running") RUNNING,
}

@Serializable
enum class GameNotice {
    @SerialName("first_d2gl_launch") FIRST_D2GL_LAUNCH,
}

@Serializable
data class Downloads(
    val complete: Boolean,
    val installers: Boolean,
    @SerialName("total_bytes") val totalBytes: Long,
)

@Serializable
data class Location(val path: String, val default: Boolean, val available: Boolean)

@Serializable
data class GameFolder(val path: String, val available: Boolean)

/** The next launch's settings. */
@Serializable
data class Settings(
    val renderer: Renderer,
    @SerialName("direct_draw_renderer") val directDrawRenderer: DirectDrawRenderer,
    @SerialName("skip_to_bnet") val skipToBnet: Boolean,
    @SerialName("sound_in_background") val soundInBackground: Boolean,
    val persistence: SettingsPersistence,
)

@Serializable
@JsonClassDiscriminator("state")
sealed class SettingsPersistence {
    @Serializable @SerialName("loading") data object Loading : SettingsPersistence()
    @Serializable @SerialName("saved") data object Saved : SettingsPersistence()

    @Serializable @SerialName("failed")
    data class Failed(val reason: String) : SettingsPersistence()
}

@Serializable
data class DownloadSource(
    val artifact: Artifact,
    @SerialName("effective_url") val effectiveUrl: String,
    val badge: SourceBadge,
    val error: String?,
    val installed: String?,
)

@Serializable
enum class SourceBadge {
    @SerialName("built_in_verified") BUILT_IN_VERIFIED,
    @SerialName("custom_from_environment") CUSTOM_FROM_ENVIRONMENT,
    @SerialName("custom") CUSTOM,
}

@Serializable
@JsonClassDiscriminator("state")
sealed class Report {
    @Serializable @SerialName("creating") data object Creating : Report()

    /** `opened`: whether its folder opened (null while that is tried). */
    @Serializable @SerialName("saved")
    data class Saved(val path: String, val opened: Boolean?) : Report()

    @Serializable @SerialName("failed")
    data class Failed(val reason: String) : Report()
}

/** A character a CD key may hold: the Launcher accepts ASCII letters, digits and `-`. */
fun isProductKeyChar(char: Char): Boolean =
    char in 'A'..'Z' || char in 'a'..'z' || char in '0'..'9' || char == '-'

/** What the Launcher accepts as a CD key: 1 to 64 [isProductKeyChar] characters. */
fun isProductKey(key: CharArray): Boolean = key.size in 1..64 && key.all(::isProductKeyChar)

/**
 * The `authorize` command message, written byte by byte from [d2Key] and [lodKey] so the keys
 * never become a String: the same bytes [ControlJson] writes for [Command.Authorize] (a CD key
 * needs no JSON escaping). The caller clears the result.
 */
internal fun authorizeCommandJson(seq: Long, d2Key: CharArray, lodKey: CharArray): ByteArray {
    require(isProductKey(d2Key) && isProductKey(lodKey)) { "not a CD key" }
    val parts = listOf(
        "{\"type\":\"command\",\"seq\":$seq,\"intent\":{\"type\":\"authorize\",\"d2_key\":\"".encodeToByteArray(),
        "\",\"lod_key\":\"".encodeToByteArray(),
        "\"}}".encodeToByteArray(),
    )
    val body = ByteArray(parts.sumOf { it.size } + d2Key.size + lodKey.size)
    var at = 0
    fun putBytes(bytes: ByteArray) {
        bytes.copyInto(body, at)
        at += bytes.size
    }
    fun putKey(key: CharArray) {
        for (char in key) body[at++] = char.code.toByte()
    }
    putBytes(parts[0])
    putKey(d2Key)
    putBytes(parts[1])
    putKey(lodKey)
    putBytes(parts[2])
    return body
}
