# Inspect only public video-channel profile UI. Never output/save profile session URL queries.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
function ProfileIdFromAddress($address) {
    try {
        $uri = [uri]$address
        if (-not $uri.IsAbsoluteUri -or $uri.Scheme -cne 'https' -or $uri.Host -ne 'channels.weixin.qq.com' -or $uri.AbsolutePath.TrimEnd('/') -ne '/web/pages/profile') { return $null }
        $ids = @()
        foreach ($pair in $uri.Query.TrimStart('?').Split('&')) {
            $parts = $pair.Split('=', 2)
            if ($parts.Count -eq 2 -and $parts[0] -ceq 'username') { $ids += [uri]::UnescapeDataString($parts[1]) }
        }
        if ($ids.Count -eq 1 -and $ids[0].Length -le 256 -and $ids[0] -cmatch '^[A-Za-z0-9_.-]+@finder$') { return $ids[0] }
    } catch { }
    return $null
}
try {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $keyword = $env:WECHAT_MONITOR_KEYWORD
    $accountId = $env:WECHAT_MONITOR_ACCOUNT_ID
    $binding = $env:WECHAT_MONITOR_BIND -eq '1'
    if (-not $keyword -or $keyword -match '[\x00-\x1f]') { 'UNREADABLE'; exit }
    if (-not $binding -and $accountId -cnotmatch '^[A-Za-z0-9_.-]+@finder$') { 'UNREADABLE'; exit }
    $descendants = [Windows.Automation.TreeScope]::Descendants
    function ByType($type) { [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, $type) }
    function ByName($name) { [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $name) }
    $roots = @()
    foreach ($process in Get-Process -Name Weixin, WeChat -ErrorAction SilentlyContinue) {
        $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty, [int]$process.Id)
        $roots += @([Windows.Automation.AutomationElement]::RootElement.FindAll([Windows.Automation.TreeScope]::Children, $condition))
    }
    function FindProfiles {
        $profiles = @()
        foreach ($root in $roots) {
            foreach ($document in $root.FindAll($descendants, (ByType ([Windows.Automation.ControlType]::Document)))) {
                if ($document.Current.IsOffscreen) { continue }
                $value = $null
                if (-not $document.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern, [ref]$value)) {
                    if (-not $document.TryGetCurrentPattern([Windows.Automation.LegacyIAccessiblePattern]::Pattern, [ref]$value)) { continue }
                }
                $id = ProfileIdFromAddress $value.Current.Value
                if (-not $id) { continue }
                $name = $document.Current.Name
                if (($binding -and $name -ceq $keyword) -or (-not $binding -and $id -ceq $accountId)) {
                    $profiles += [pscustomobject]@{ Document = $document; Root = $root; AccountId = $id; Name = $name }
                }
            }
        }
        return $profiles
    }
    $profiles = @(FindProfiles)
    # An inactive tab is just a navigation hint. Identity MUST be checked again
    # from the profile document URL after selection; never trust the tab name.
    if (-not $binding -and $profiles.Count -eq 0) {
        $tabs = @()
        foreach ($root in $roots) {
            foreach ($tab in $root.FindAll($descendants, (ByType ([Windows.Automation.ControlType]::TabItem)))) {
                if ($tab.Current.Name -ceq $keyword) { $tabs += $tab }
            }
        }
        if ($tabs.Count -gt 1) { 'AMBIGUOUS'; exit }
        if ($tabs.Count -eq 1) {
            $pattern = $null
            if ($tabs[0].TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) { $pattern.Select() }
            elseif ($tabs[0].TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { $pattern.Invoke() }
            for ($attempt = 0; $attempt -lt 10 -and $profiles.Count -eq 0; $attempt++) {
                Start-Sleep -Milliseconds 200
                $profiles = @(FindProfiles)
            }
        }
    }
    if ($profiles.Count -eq 0) { 'NOPAGE'; exit }
    if ($profiles.Count -ne 1) { 'AMBIGUOUS'; exit }
    $profile = $profiles[0]
    if ($binding) {
        $name = $profile.Name.Replace("`t", ' ').Replace("`r", ' ').Replace("`n", ' ')
        "PROFILE`t$($profile.AccountId)`t$name"
        exit
    }
    if ($env:WECHAT_MONITOR_REFRESH -eq '1') {
        $reloadCondition = [Windows.Automation.AndCondition]::new((ByType ([Windows.Automation.ControlType]::Button)), (ByName '重新加载'))
        $reload = $profile.Root.FindFirst($descendants, $reloadCondition)
        if ($null -eq $reload) {
            $moreCondition = [Windows.Automation.AndCondition]::new((ByType ([Windows.Automation.ControlType]::Button)), (ByName '更多'))
            $more = @($profile.Root.FindAll($descendants, $moreCondition))
            $pattern = $null
            if ($more.Count -eq 1 -and $more[0].TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
                $pattern.Invoke()
                Start-Sleep -Milliseconds 200
                $reload = $profile.Root.FindFirst($descendants, $reloadCondition)
            }
        }
        $pattern = $null
        if ($null -eq $reload -or -not $reload.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { 'REFRESH_FAILED'; exit }
        $pattern.Invoke()
        Start-Sleep -Seconds 3
        $profiles = @(FindProfiles)
        if ($profiles.Count -ne 1) { 'UNREADABLE'; exit }
        $profile = $profiles[0]
    }
    $page = $profile.Document
    $liveControls = @()
    foreach ($element in $page.FindAll($descendants, [Windows.Automation.Condition]::TrueCondition)) {
        if ($element.Current.Name -cmatch '^(直播中|正在直播)(?:\s|$)') { $liveControls += $element }
        if ($element.Current.Name -cmatch '^(加载中|正在加载)(?:[.。…\s]|$)') { 'UNREADABLE'; exit }
    }
    if ($liveControls.Count -gt 0) {
        if ($env:WECHAT_MONITOR_ENTER -eq '1') {
            $entered = $false
            foreach ($control in $liveControls) {
                $pattern = $null
                if ($control.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { $pattern.Invoke(); $entered = $true; break }
            }
            if (-not $entered) { 'UNREADABLE'; exit }
        }
        "LIVE`t$accountId`t$keyword"
    } else {
        "OFFLINE`t$accountId"
    }
} catch {
    # No exception text: it could contain a signed profile URL.
    'UNREADABLE'
}
