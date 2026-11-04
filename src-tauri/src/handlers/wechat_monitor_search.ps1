# UI accessibility only: no private API calls, chat reading, or mouse clicks.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
try {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $keyword = $env:WECHAT_MONITOR_KEYWORD
    if (-not $keyword -or $keyword -match '[\x00-\x1f]') { 'UNREADABLE'; exit }
    $pageName = $keyword + ' - 搜一搜'
    $descendants = [Windows.Automation.TreeScope]::Descendants
    function ByType($type) { [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, $type) }
    function ByName($name) { [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $name) }
    function NormalName($name) { ($name -replace '\s', '').Trim() }
    $roots = @()
    # WeChat's browser can be a separate top-level window. MainWindowHandle alone
    # misses it and incorrectly yields NOPAGE when another tab/window is active.
    foreach ($process in Get-Process -Name Weixin, WeChat -ErrorAction SilentlyContinue) {
        $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty, [int]$process.Id)
        $roots += @([Windows.Automation.AutomationElement]::RootElement.FindAll([Windows.Automation.TreeScope]::Children, $condition))
    }
    function FindPage {
        $pages = @()
        foreach ($root in $roots) {
            foreach ($document in $root.FindAll($descendants, (ByType ([Windows.Automation.ControlType]::Document)))) {
                if ((NormalName $document.Current.Name) -eq (NormalName $pageName)) { $pages += $document }
            }
        }
        return $pages
    }
    $pages = @(FindPage)
    if ($pages.Count -eq 0) {
        foreach ($root in $roots) {
            foreach ($tab in $root.FindAll($descendants, (ByType ([Windows.Automation.ControlType]::TabItem)))) {
                if ((NormalName $tab.Current.Name) -ne (NormalName $pageName)) { continue }
                $pattern = $null
                if ($tab.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) {
                    $pattern.Select()
                } elseif ($tab.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
                    $pattern.Invoke()
                }
            }
        }
        for ($attempt = 0; $attempt -lt 10 -and $pages.Count -eq 0; $attempt++) {
            Start-Sleep -Milliseconds 200
            $pages = @(FindPage)
        }
    }
    if ($pages.Count -eq 0) { 'NOPAGE'; exit }
    if ($pages.Count -ne 1) { 'AMBIGUOUS'; exit }
    $page = $pages[0]
    if ($env:WECHAT_MONITOR_REFRESH -eq '1') {
        $searchButton = [Windows.Automation.AndCondition]::new((ByType ([Windows.Automation.ControlType]::Button)), (ByName '搜索'))
        $button = $page.FindFirst($descendants, $searchButton)
        $pattern = $null
        if ($null -eq $button -or -not $button.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
            'REFRESH_FAILED'; exit
        }
        $pattern.Invoke()
        Start-Sleep -Seconds 3
        $pages = @(FindPage)
        if ($pages.Count -ne 1) { 'UNREADABLE'; exit }
        $page = $pages[0]
    }
    $liveItems = @()
    foreach ($item in $page.FindAll($descendants, (ByType ([Windows.Automation.ControlType]::ListItem)))) {
        if (-not $item.Current.Name.StartsWith('直播中')) { continue }
        # Never use EndsWith(keyword): it also accepts names such as "工作室陈赫".
        $exactToken = $item.Current.Name -cmatch ('(?:^|\s)' + [regex]::Escape($keyword) + '$')
        if ($exactToken) { $liveItems += $item }
    }
    if ($liveItems.Count -gt 1) { 'AMBIGUOUS'; exit }
    if ($liveItems.Count -eq 1) {
        $item = $liveItems[0]
        $title = $item.Current.Name.Replace("`t", ' ').Replace("`r", ' ').Replace("`n", ' ')
        if ($env:WECHAT_MONITOR_ENTER -eq '1') {
            $pattern = $null
            if (-not $item.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { 'UNREADABLE'; exit }
            $pattern.Invoke()
        }
        "LIVE`t$keyword`t$title"
        exit
    }
    $matches = @()
    foreach ($text in $page.FindAll($descendants, (ByType ([Windows.Automation.ControlType]::Text)))) {
        if ($text.Current.Name -ceq $keyword) { $matches += $text }
    }
    if ($matches.Count -gt 1) { 'AMBIGUOUS'; exit }
    if ($matches.Count -eq 0) { 'NOTFOUND'; exit }
    # This describes the observed UI, not an authoritative server-side offline API.
    "OFFLINE`t$keyword"
} catch {
    # Provider/layout errors must not masquerade as a confirmed offline account.
    'UNREADABLE'
}
