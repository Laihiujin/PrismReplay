function Assert-CudaDelayImports {
    param([Parameter(Mandatory = $true)][string]$Dependents)

    $regular = [System.Collections.Generic.List[string]]::new()
    $delayed = [System.Collections.Generic.List[string]]::new()
    $section = ''
    foreach ($line in ($Dependents -split "`r?`n")) {
        if ($line -match 'following delay load dependencies:') {
            $section = 'delayed'
        } elseif ($line -match 'following dependencies:') {
            $section = 'regular'
        } elseif ($line -match '^\s*Summary\s*$') {
            $section = ''
        } elseif ($line -match '^\s*([\w.-]+\.dll)\s*$') {
            $dll = $matches[1].ToLowerInvariant()
            if ($section -eq 'regular') { $regular.Add($dll) }
            if ($section -eq 'delayed') { $delayed.Add($dll) }
        }
    }

    if (-not $delayed.Contains('cublas64_12.dll')) {
        throw 'cublas64_12.dll must be delay-loaded.'
    }
    # The linker can discard unused cublasLt imports. If present, it must not
    # be eager-loaded. nvcuda is optional until GPU transcription starts too.
    foreach ($dll in @('cublas64_12.dll', 'cublaslt64_12.dll', 'nvcuda.dll')) {
        if ($regular.Contains($dll)) { throw "$dll is an eager dependency." }
    }
    [PSCustomObject]@{ Regular = $regular.ToArray(); Delayed = $delayed.ToArray() }
}
