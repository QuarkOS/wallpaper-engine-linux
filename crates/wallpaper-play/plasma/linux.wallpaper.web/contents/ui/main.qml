import QtQuick
import QtWebEngine
import org.kde.plasma.plasmoid

WallpaperItem {
    id: root

    // Importing QtWebEngine initializes the engine in plasmashell. Plasma 6
    // has no stock web wallpaper, so this view is the page.
    //
    // Do not bind WebEngineView.url to PageUrl. The view writes url while
    // it navigates, which drops the binding and leaves the first value in
    // place. That first value is empty until the shell script finishes, and
    // an empty view paints nothing. The containment behind it is black, so
    // the desktop stays black. An empty PageUrl must not replace a page
    // that already loaded.
    readonly property string configuredPage: root.configuration.PageUrl || ""
    property string shownPage: ""

    onConfiguredPageChanged: Qt.callLater(applyPageUrl)

    // A wallpaper that starts with sound is the wrong default. The shell
    // script also writes Muted; this binding keeps later config edits live.
    // Muted true (the default, and an unset value) mutes the page. Muted
    // false leaves the page audio on. Muting does not hide the page or
    // replace it.
    WebEngineView {
        id: page
        x: 0
        y: 0
        width: root.width
        height: root.height

        // Behind the document only. The default would be white. Opaque black
        // is the whole desktop when the document has not painted. Transparent
        // lets the loaded page show its own background.
        backgroundColor: "transparent"
        audioMuted: root.configuration.Muted !== false

        // Set before the page URL is applied. A file:// page cannot load a
        // sibling script, style sheet, or image unless local access is on.
        // Those assets are what paint the page; without them the view stays
        // empty. Remote assets referenced by that page are allowed too.
        settings.javascriptEnabled: true
        settings.localContentCanAccessFileUrls: true
        settings.localContentCanAccessRemoteUrls: true
        settings.playbackRequiresUserGesture: false

        // A discarded or frozen wallpaper paints nothing.
        onRecommendedStateChanged: lifecycleState = WebEngineView.LifecycleState.Active

        onWidthChanged: root.applyPageUrl()
        onHeightChanged: root.applyPageUrl()

        onLoadingChanged: function(loadRequest) {
            if (loadRequest.status === WebEngineView.LoadSucceededStatus) {
                lifecycleState = WebEngineView.LifecycleState.Active
            }
            if (loadRequest.errorString) {
                console.warn("linux.wallpaper.web:", loadRequest.errorString)
            }
        }
    }

    function applyPageUrl() {
        var next = configuredPage
        if (next === "") {
            return
        }
        // A 0×0 view records a black frame and does not repaint when the
        // containment later receives the screen size.
        if (page.width < 1 || page.height < 1) {
            return
        }
        if (shownPage === next) {
            return
        }
        page.url = next
        shownPage = next
    }

    Component.onCompleted: {
        page.lifecycleState = WebEngineView.LifecycleState.Active
        applyPageUrl()
    }
}
