package com.pd2launcher.app.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.pd2launcher.app.DeviceCheck
import com.pd2launcher.app.GameFilesProvider
import com.pd2launcher.app.LauncherSettings
import com.pd2launcher.app.R
import com.pd2launcher.app.SessionPaths
import com.pd2launcher.app.SessionService

/**
 * A program path input (Game program, Launcher program): the [title], the line that says what
 * the program is ([intro]), the fixed "Diablo II/" text before the field with the path now
 * ([current]), one help line ([help]), and Default, Cancel and OK ([SettingTextInput]). Default
 * puts [default] in the field. OK takes a path that the engine takes
 * ([LauncherSettings.typedProgramPath]) of a file that is in the game folder
 * ([LauncherSettings.programMissing]) and gives it to [onSet]; another one turns the help line into
 * its reason, in amber, and the input stays. The engine checks again, and its refusal wins.
 */
@Composable
internal fun ProgramPathInput(
    title: String,
    intro: String,
    help: String,
    current: String,
    default: String,
    onSet: (String) -> Unit,
    onClose: () -> Unit,
) {
    val context = LocalContext.current
    val problems = LauncherSettings.PROGRAM_PROBLEMS.associateWith { stringResource(LauncherSettings.programPathProblem(it), LauncherSettings.PROGRAM_PATH_MAX) }
    SettingTextInput(
        title = title,
        intro = intro,
        prefix = stringResource(R.string.program_folder),
        current = current,
        maxLength = LauncherSettings.PROGRAM_PATH_MAX,
        empty = default,
        help = help,
        reset = stringResource(R.string.settings_program_default) to default,
        accept = { typed ->
            when (val program = LauncherSettings.typedProgramPath(typed)) {
                is LauncherSettings.TypedProgram.Refused -> problems.getValue(program.reason)
                is LauncherSettings.TypedProgram.Path -> {
                    // The game folder as the engine named it; null while it is not known (the engine checks).
                    val folder = GameFilesProvider.gameFolder(SessionPaths(DeviceCheck.dataDir(context)), SessionService.status.value.snapshot?.gameFolder?.path)
                    if (LauncherSettings.programMissing(folder, program.path)) {
                        problems.getValue(LauncherSettings.PROGRAM_MISSING)
                    } else {
                        if (program.path != current) onSet(program.path)
                        null
                    }
                }
            }
        },
        onClose = onClose,
    )
}
