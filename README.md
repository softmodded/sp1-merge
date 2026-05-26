# sp1-merge

**(you should probably use the [website](https://sp1.clefairy.org/). this cli is if you're familiar with the terminal and need to bulk-encode songs)**  

a rust cli tool for merging stem files created with [demucs](https://github.com/CarlGao4/Demucs-Gui) into a .wav file compatible with the teenage engineering sp-01  
  
the resulting wav file is able to be uploaded to any sp-01 using the [web stem loader](https://solderless.engineering)  

## how to use:
<img width="791" height="288" alt="image" src="https://github.com/user-attachments/assets/e7dc56bf-1f98-4636-8992-3a5b584fb19c" />  
`sp1-merge` - starts the program  
`sp1-merge config` - opens configuration editor  
  
## building  
requirements: 
- [rust](https://rust-lang.org/learn/get-started/)  
  
clone the repo:
`git clone https://github.com/softmodded/sp1-merge`  
  
move into directory:  
`cd sp1-merge`  
  
build with cargo:  
`cargo build`  
