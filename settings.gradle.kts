pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "pd2launcher-android"
include(":app")
// Development apps built only by `build.sh mockup` and `build.sh spike`; the public source has neither.
if (settingsDir.resolve("mockup").isDirectory) include(":mockup")
if (settingsDir.resolve("spike").isDirectory) include(":spike")
